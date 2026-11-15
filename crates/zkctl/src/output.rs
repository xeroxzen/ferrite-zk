//! Output: the machine formats (JSON and NDJSON, stable and documented in
//! docs/GUIDE.md) and plain tables for people.

use std::io::Write;

use anyhow::Result;
use chrono::{DateTime, NaiveDateTime, SecondsFormat, Utc};
use serde::Serialize;
use zkteco_client::zkteco_proto::packet::Packet;
use zkteco_client::{Attendance, CapturedEvent, LiveEvent, Privilege, Source, User, UserIdSource};

use crate::event_id::{self, DeviceKey};
use zkteco_proto::records::attendance::{AttendanceStatus, Punch};

use crate::cli::Format;

/// Device-local time: `YYYY-MM-DDTHH:MM:SS`, no zone.
#[must_use]
pub fn device_time(t: NaiveDateTime) -> String {
    t.format("%Y-%m-%dT%H:%M:%S").to_string()
}

/// Host time: RFC 3339 in UTC with milliseconds.
#[must_use]
pub fn host_time(t: std::time::SystemTime) -> String {
    DateTime::<Utc>::from(t).to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// A user, without the PIN.
#[derive(Debug, Clone, Serialize)]
pub struct UserOut {
    /// Device slot.
    pub uid: u16,
    /// User ID.
    pub user_id: String,
    /// Name.
    pub name: String,
    /// `user`, `enroller`, `manager`, `admin`, or `unknown:<n>`.
    pub privilege: String,
    /// Disabled.
    pub disabled: bool,
    /// Card number (0 for none).
    pub card: u32,
    /// Group.
    pub group_id: String,
    /// Whether a PIN is set (the PIN itself is never printed).
    pub has_password: bool,
}

impl From<&User> for UserOut {
    fn from(u: &User) -> Self {
        Self {
            uid: u.uid.0,
            user_id: u.user_id.as_str().to_owned(),
            name: u.name.clone(),
            privilege: privilege_name(u.privilege),
            disabled: u.disabled,
            card: u.card,
            group_id: u.group_id.clone(),
            has_password: !u.password.is_empty(),
        }
    }
}

/// The name of a role.
#[must_use]
pub fn privilege_name(p: Privilege) -> String {
    match p {
        Privilege::User => "user".into(),
        Privilege::Enroller => "enroller".into(),
        Privilege::Manager => "manager".into(),
        Privilege::Admin => "admin".into(),
        Privilege::Unknown(n) => format!("unknown:{n}"),
    }
}

/// Version of the event schema (docs/schema/event-v1.json). Bumped only
/// for changes that break consumers; added fields do not bump it.
pub const SCHEMA_VERSION: u32 = 1;

/// One attendance record or live event: the object `attendance pull` and
/// `live` print, the same for every source (DECISIONS.md D34). Every key is
/// always present, except `raw`, which only undecoded events have.
#[derive(Debug, Clone, Serialize)]
pub struct EventOut {
    /// Schema version, [`SCHEMA_VERSION`].
    pub v: u32,
    /// Deterministic identifier: the same punch has the same `id` whether
    /// pulled, live or backfilled ([`crate::event_id`]).
    pub id: String,
    /// The device (profile name or host).
    pub device: String,
    /// `pull`, `live`, or `backfill` (read from the log by `live`).
    pub source: &'static str,
    /// Device clock in the record, device-local, no zone; `null` for raw
    /// events.
    pub device_time: Option<String>,
    /// Host time the event arrived (RFC 3339, UTC); `null` for `pull`.
    pub received_at: Option<String>,
    /// `attendance`, or `raw` for an event this version does not decode.
    pub kind: &'static str,
    /// Device slot, if known.
    pub uid: Option<u16>,
    /// User ID, if known.
    pub user_id: Option<String>,
    /// Where `user_id` came from: `record` (the record itself) or
    /// `user_table` (looked up by uid); `null` when unresolved, and for raw
    /// events.
    pub user_id_source: Option<&'static str>,
    /// Verification method: `password`, `fingerprint`, `card`, `face` or
    /// `unknown`.
    pub status: Option<&'static str>,
    /// The raw method code.
    pub status_code: Option<u8>,
    /// `check_in`, `check_out`, `break_out`, `break_in`, `overtime_in`,
    /// `overtime_out` or `unknown`.
    pub punch: Option<&'static str>,
    /// The raw punch code.
    pub punch_code: Option<u8>,
    /// Work code, when the layout has one.
    pub workcode: Option<u32>,
    /// For `kind: raw` only: the undecoded event.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<RawOut>,
}

/// An event that was not decoded.
#[derive(Debug, Clone, Serialize)]
pub struct RawOut {
    /// The event flags.
    pub flags: u32,
    /// The payload, hex.
    pub payload_hex: String,
}

impl EventOut {
    /// A record read by `attendance pull` from `device` (identified by
    /// `key`), with where its user ID came from.
    #[must_use]
    pub fn pulled(device: &str, key: &DeviceKey, a: &Attendance, user_id_source: UserIdSource) -> Self {
        Self::attendance(device, key, "pull", None, a, user_id_source)
    }

    /// An event from `live`, from `device` (identified by `key`).
    #[must_use]
    pub fn captured(device: &str, key: &DeviceKey, e: &CapturedEvent) -> Self {
        let source = match e.source {
            Source::Live => "live",
            Source::Backfill => "backfill",
        };
        let received_at = Some(host_time(e.received_at));
        match &e.event {
            LiveEvent::Attendance(a) => {
                let user_id_source = e.user_id_source.unwrap_or(UserIdSource::Unresolved);
                Self::attendance(device, key, source, received_at, a, user_id_source)
            }
            LiveEvent::Raw { flag, payload } => Self {
                v: SCHEMA_VERSION,
                id: event_id::raw(key, flag.bits(), payload),
                device: device.to_owned(),
                source,
                device_time: None,
                received_at,
                kind: "raw",
                uid: None,
                user_id: None,
                user_id_source: None,
                status: None,
                status_code: None,
                punch: None,
                punch_code: None,
                workcode: None,
                raw: Some(RawOut {
                    flags: flag.bits(),
                    payload_hex: hex::encode(payload),
                }),
            },
        }
    }

    /// The same event with a user ID that came from the user table left
    /// out, as if unresolved. `id` is unchanged: it is computed from the
    /// resolved person, so it matches live events (`attendance pull`
    /// without `--with-users`).
    #[must_use]
    pub fn without_resolved_user_id(mut self) -> Self {
        if self.user_id_source == Some("user_table") {
            self.user_id = None;
            self.user_id_source = None;
        }
        self
    }

    fn attendance(
        device: &str,
        key: &DeviceKey,
        source: &'static str,
        received_at: Option<String>,
        a: &Attendance,
        user_id_source: UserIdSource,
    ) -> Self {
        Self {
            v: SCHEMA_VERSION,
            id: event_id::attendance(key, a),
            device: device.to_owned(),
            source,
            device_time: Some(device_time(a.timestamp)),
            received_at,
            kind: "attendance",
            uid: a.uid.map(|u| u.0),
            user_id: a.user_id.as_ref().map(|id| id.as_str().to_owned()),
            user_id_source: match (user_id_source, &a.user_id) {
                (_, None) | (UserIdSource::Unresolved, _) => None,
                (UserIdSource::Record, Some(_)) => Some("record"),
                (UserIdSource::UserTable, Some(_)) => Some("user_table"),
            },
            status: Some(status_name(a.status)),
            status_code: Some(a.status.to_byte()),
            punch: Some(punch_name(a.punch)),
            punch_code: Some(a.punch.to_byte()),
            workcode: a.workcode,
            raw: None,
        }
    }
}

fn status_name(s: AttendanceStatus) -> &'static str {
    match s {
        AttendanceStatus::Password => "password",
        AttendanceStatus::Fingerprint => "fingerprint",
        AttendanceStatus::Card => "card",
        AttendanceStatus::Face => "face",
        AttendanceStatus::Unknown(_) => "unknown",
    }
}

fn punch_name(p: Punch) -> &'static str {
    match p {
        Punch::CheckIn => "check_in",
        Punch::CheckOut => "check_out",
        Punch::BreakOut => "break_out",
        Punch::BreakIn => "break_in",
        Punch::OvertimeIn => "overtime_in",
        Punch::OvertimeOut => "overtime_out",
        Punch::Unknown(_) => "unknown",
    }
}

/// A packet a dry run would have sent.
#[derive(Debug, Clone, Serialize)]
pub struct PlannedOut {
    /// Command name.
    pub command: String,
    /// Command code.
    pub code: u16,
    /// Payload bytes.
    pub payload_len: usize,
    /// Payload, hex; `null` when it holds biometric data.
    pub payload_hex: Option<String>,
}

/// Describe planned packets; `redact` leaves payloads out.
#[must_use]
pub fn planned(packets: &[Packet], redact: bool) -> Vec<PlannedOut> {
    packets
        .iter()
        .map(|p| PlannedOut {
            command: format!("{:?}", p.header.command),
            code: u16::from(p.header.command),
            payload_len: p.payload.len(),
            payload_hex: (!redact).then(|| hex::encode(&p.payload)),
        })
        .collect()
}

/// Print a dry run's packets.
///
/// # Errors
///
/// If stdout fails.
pub fn print_planned(format: Format, packets: &[Packet], redact: bool) -> Result<()> {
    let plan = planned(packets, redact);
    match format {
        Format::Json => json(&serde_json::json!({ "dry_run": true, "would_send": plan })),
        Format::Ndjson => {
            for p in &plan {
                ndjson(p)?;
            }
            Ok(())
        }
        Format::Human => {
            let mut out = std::io::stdout().lock();
            writeln!(out, "Dry run: nothing was changed. Would send:")?;
            for p in &plan {
                let payload = p.payload_hex.as_deref().unwrap_or("(biometric data, not shown)");
                writeln!(
                    out,
                    "  {} ({}): {} bytes {}",
                    p.command, p.code, p.payload_len, payload
                )?;
            }
            Ok(())
        }
    }
}

/// Print one pretty JSON document.
///
/// # Errors
///
/// If stdout fails.
pub fn json(value: &impl Serialize) -> Result<()> {
    let mut out = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut out, value)?;
    writeln!(out)?;
    Ok(())
}

/// Print one JSON line and flush, so consumers see it at once.
///
/// # Errors
///
/// If stdout fails.
pub fn ndjson(value: &impl Serialize) -> Result<()> {
    let mut out = std::io::stdout().lock();
    serde_json::to_writer(&mut out, value)?;
    writeln!(out)?;
    out.flush()?;
    Ok(())
}

/// A plain table: columns padded to their widest cell.
#[must_use]
pub fn table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    let line = |cells: &mut dyn Iterator<Item = &str>| {
        let padded: Vec<String> = cells.zip(&widths).map(|(c, w)| format!("{c:<w$}")).collect();
        padded.join("  ").trim_end().to_owned()
    };
    let mut text = line(&mut headers.iter().copied());
    for row in rows {
        text.push('\n');
        text.push_str(&line(&mut row.iter().map(String::as_str)));
    }
    text
}

/// Print `text` and a newline to stdout.
///
/// # Errors
///
/// If stdout fails.
pub fn text(text: &str) -> Result<()> {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{text}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_pads_columns() {
        let t = table(&["A", "LONG"], &[vec!["xyz".into(), "1".into()]]);
        assert_eq!(t, "A    LONG\nxyz  1");
    }
}
