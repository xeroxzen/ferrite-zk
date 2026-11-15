//! `zkctl attendance pull|clear`.

use std::time::Duration;

use anyhow::{Context, Result};
use chrono::NaiveDateTime;
use zkteco_client::zkteco_proto::records::RejectedRecord;
use zkteco_client::{Attendance, Client, UserIdSource};

use super::cursor::{self, Cursor};
use crate::cli::{AttendanceCommand, ClearArgs, Format, Global, PullArgs};
use crate::duration::parse_duration;
use crate::event_id::DeviceKey;
use crate::exit::{classify, Exit, Fail};
use crate::output::{self, EventOut};
use crate::target::{self, Access, Target};

pub(super) async fn run(g: &Global, cmd: AttendanceCommand) -> Result<Exit> {
    match cmd {
        AttendanceCommand::Pull(args) => pull(g, &args).await,
        AttendanceCommand::Clear(args) => {
            let target = target::single(g)?;
            super::danger::clear(g, &target, &args, false).await
        }
    }
}

/// A time bound: absolute, or a duration before the device's clock.
#[derive(Debug, Clone, Copy)]
enum Bound {
    At(NaiveDateTime),
    Ago(Duration),
}

fn parse_bound(text: &str) -> Result<Bound> {
    if let Ok(d) = parse_duration(text) {
        return Ok(Bound::Ago(d));
    }
    super::time::parse_time(text).map(Bound::At)
}

/// One device's records.
struct Pulled {
    /// What identifies the device in event ids.
    key: DeviceKey,
    records: Vec<Attendance>,
    /// Where each record's user ID came from, by index.
    sources: Vec<UserIdSource>,
    rejected: Vec<RejectedRecord>,
    /// The device clock, read when a bound is relative.
    now: Option<NaiveDateTime>,
}

async fn pull(g: &Global, args: &PullArgs) -> Result<Exit> {
    let since = args.since.as_deref().map(parse_bound).transpose()?;
    let until = args.until.as_deref().map(parse_bound).transpose()?;
    let relative = matches!(since, Some(Bound::Ago(_))) || matches!(until, Some(Bound::Ago(_)));
    let targets = target::resolve(g)?;
    let mut state = args.state.as_deref().map(cursor::load).transpose()?;
    let mut tasks = tokio::task::JoinSet::new();
    for (i, t) in targets.iter().enumerate() {
        let t = t.clone();
        tasks.spawn(async move { (i, read(&t, relative).await) });
    }
    let mut results: Vec<Option<Result<Pulled>>> = (0..targets.len()).map(|_| None).collect();
    while let Some(joined) = tasks.join_next().await {
        let (i, result) = joined.context("a device task failed")?;
        results[i] = Some(result);
    }
    let mut exit = Exit::Ok;
    let mut out = Vec::new();
    for (t, result) in targets.iter().zip(results) {
        match result.context("missing device result")? {
            Ok(pulled) => {
                let cursor = state.as_ref().and_then(|s| s.devices.get(&t.name));
                let (records, last) = select(&pulled, cursor, since, until, &t.name, args.with_users);
                exit = exit.worst(report_rejected(&t.name, "attendance", &pulled.rejected));
                if let (Some(state), Some(last)) = (state.as_mut(), last) {
                    state.devices.insert(t.name.clone(), last);
                }
                out.extend(records);
            }
            Err(e) => {
                eprintln!("error: {}: {e:#}", t.name);
                exit = exit.worst(classify(&e));
            }
        }
    }
    print(g.output, &out)?;
    if let (Some(path), Some(state)) = (&args.state, &state) {
        cursor::save(path, state)?;
    }
    Ok(exit)
}

async fn read(t: &Target, relative: bool) -> Result<Pulled> {
    let mut client = target::connect(t, Access::ReadOnly).await?;
    let key = DeviceKey::read(&mut client, &t.name).await;
    let pulled = read_from(&mut client, key, relative)
        .await
        .with_context(|| format!("reading attendance from {}", t.name))?;
    if let Err(e) = client.disconnect().await {
        tracing::warn!(%e, device = t.name, "disconnect");
    }
    Ok(pulled)
}

/// Records are always resolved against the user table, which is read only
/// when some record lacks a user ID, so that `id` names the person the
/// way live events do (DECISIONS.md D36). `--with-users` decides only
/// whether the resolved user IDs are shown ([`select`]).
async fn read_from(client: &mut Client, key: DeviceKey, relative: bool) -> zkteco_client::Result<Pulled> {
    let now = if relative {
        Some(client.time().await?)
    } else {
        None
    };
    let batch = client.attendance_with_users().await?;
    Ok(Pulled {
        key,
        sources: batch.records.iter().map(|r| r.source).collect(),
        records: batch.records.into_iter().map(|r| r.record).collect(),
        rejected: batch.rejected,
        now,
    })
}

/// The records to emit, in log order, and the new cursor.
fn select(
    pulled: &Pulled,
    cursor: Option<&Cursor>,
    since: Option<Bound>,
    until: Option<Bound>,
    device: &str,
    with_users: bool,
) -> (Vec<EventOut>, Option<Cursor>) {
    let resolve = |b: Bound| match b {
        Bound::At(t) => Some(t),
        Bound::Ago(d) => {
            let d = chrono::Duration::from_std(d).ok()?;
            pulled.now.and_then(|now| now.checked_sub_signed(d))
        }
    };
    let since = since.and_then(resolve);
    let until = until.and_then(resolve);
    let mut last = None;
    let mut out = Vec::new();
    for i in cursor::after(&pulled.records, cursor, device) {
        let a = &pulled.records[i];
        if since.is_some_and(|s| a.timestamp < s) || until.is_some_and(|u| a.timestamp >= u) {
            continue;
        }
        last = Some(Cursor::of(a));
        let event = EventOut::pulled(device, &pulled.key, a, pulled.sources[i]);
        out.push(if with_users {
            event
        } else {
            event.without_resolved_user_id()
        });
    }
    (out, last)
}

fn print(format: Format, records: &[EventOut]) -> Result<()> {
    match format {
        Format::Json => output::json(&records),
        Format::Ndjson => records.iter().try_for_each(output::ndjson),
        Format::Human => {
            let rows: Vec<Vec<String>> = records
                .iter()
                .map(|r| {
                    vec![
                        r.device.clone(),
                        r.device_time.clone().unwrap_or_default(),
                        r.user_id.clone().unwrap_or_default(),
                        r.uid.map(|u| u.to_string()).unwrap_or_default(),
                        r.status.unwrap_or_default().to_owned(),
                        r.punch.unwrap_or_default().to_owned(),
                    ]
                })
                .collect();
            output::text(&output::table(
                &["DEVICE", "TIME", "USER ID", "UID", "METHOD", "PUNCH"],
                &rows,
            ))
        }
    }
}

/// Warn about rejected records on stderr; partial success if any.
pub(super) fn report_rejected(device: &str, table: &str, rejected: &[RejectedRecord]) -> Exit {
    for r in rejected {
        eprintln!(
            "warning: {device}: {table} record at offset {} rejected: {}",
            r.offset, r.reason
        );
    }
    if rejected.is_empty() {
        Exit::Ok
    } else {
        Exit::Partial
    }
}

/// Fail unless the device has a name to type: used by the clears.
pub(super) fn expect_name(args: &ClearArgs, name: &str, what: &str) -> Result<()> {
    if let Some(given) = &args.confirm_device {
        if given == name {
            return Ok(());
        }
        return Err(Fail::aborted(format!(
            "--confirm-device {given:?} does not match {name:?}; nothing done"
        )));
    }
    if !crate::secrets::interactive() {
        return Err(Fail::aborted(format!(
            "{what} cannot be undone; confirm with --confirm-device {name} (nothing done)"
        )));
    }
    eprintln!(
        "{what} cannot be undone. Export first: `zkctl attendance pull` (and `zkctl templates export`)."
    );
    eprint!("Type the device name ({name}) to confirm: ");
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .context("reading the confirmation")?;
    if line.trim() == name {
        Ok(())
    } else {
        Err(Fail::aborted("the name did not match; nothing done"))
    }
}
