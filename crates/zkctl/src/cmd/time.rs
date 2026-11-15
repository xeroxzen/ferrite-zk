//! `zkctl time get|set`.

use anyhow::Result;
use chrono::{DateTime, Local, NaiveDateTime, Timelike};
use serde::Serialize;

use crate::cli::{Format, Global, TimeCommand};
use crate::exit::{Exit, Fail};
use crate::output;
use crate::target::{self, Access};

#[derive(Debug, Serialize)]
struct TimeOut {
    device: String,
    time: String,
}

pub(super) async fn run(g: &Global, cmd: TimeCommand) -> Result<Exit> {
    super::one_shot(g)?;
    let target = target::single(g)?;
    match cmd {
        TimeCommand::Get => {
            let mut client = target::connect(&target, Access::ReadOnly).await?;
            let time = client.time().await?;
            if let Err(e) = client.disconnect().await {
                tracing::warn!(%e, "disconnect");
            }
            print(g.output, &target.name, time)?;
        }
        TimeCommand::Set { time, sync, confirm } => {
            super::require_yes(&confirm, "time set")?;
            let new = if sync {
                now()
            } else {
                parse_time(time.as_deref().unwrap_or_default())?
            };
            let access = Access::Write {
                dry_run: confirm.dry_run,
            };
            let mut client = target::connect(&target, access).await?;
            client.set_time(new).await?;
            if confirm.dry_run {
                output::print_planned(g.output, &client.take_planned(), false)?;
            } else {
                print(g.output, &target.name, new)?;
            }
            if let Err(e) = client.disconnect().await {
                tracing::warn!(%e, "disconnect");
            }
        }
    }
    Ok(Exit::Ok)
}

fn print(format: Format, device: &str, time: NaiveDateTime) -> Result<()> {
    match format {
        Format::Human => output::text(&output::device_time(time)),
        _ => output::json(&TimeOut {
            device: device.to_owned(),
            time: output::device_time(time),
        }),
    }
}

/// This computer's local time, to the second.
fn now() -> NaiveDateTime {
    let t = Local::now().naive_local();
    t.with_nanosecond(0).unwrap_or(t)
}

/// A time on the command line: RFC 3339 (converted to this computer's
/// time zone, on the assumption that the device clock follows it) or a
/// device-local `YYYY-MM-DDTHH:MM:SS`.
///
/// # Errors
///
/// A usage error for anything else.
pub(super) fn parse_time(text: &str) -> Result<NaiveDateTime> {
    if let Ok(t) = DateTime::parse_from_rfc3339(text) {
        return Ok(t.with_timezone(&Local).naive_local());
    }
    NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S").map_err(|_| {
        Fail::usage(format!(
            "{text:?} is not a time: use RFC 3339 (2026-01-02T08:00:00+02:00) or device-local 2026-01-02T08:00:00"
        ))
    })
}
