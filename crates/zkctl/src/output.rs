//! Output: the machine formats (JSON and NDJSON, stable and documented in
//! docs/GUIDE.md) and plain tables for people.

use std::io::Write;

use anyhow::Result;
use chrono::{DateTime, NaiveDateTime, SecondsFormat, Utc};
use serde::Serialize;
use zkteco_client::zkteco_proto::packet::Packet;
use zkteco_client::{Attendance, CapturedEvent, LiveEvent, Privilege, Source, User, UserIdSource};
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

/// The fields of an attendance record, shared by `attendance pull` and
/// `live`.
#[derive(Debug, Clone, Serialize)]
pub struct Punched {
    /// Device slot, if known.
    pub uid: Option<u16>,
    /// User ID, if known.
    pub user_id: Option<String>,
    /// Verification method: `password`, `fingerprint`, `card`, `face` or
    /// `unknown`.
    pub status: &'static str,
    /// The raw method code.
    pub status_code: u8,
    /// `check_in`, `check_out`, `break_out`, `break_in`, `overtime_in`,
    /// `overtime_out` or `unknown`.
    pub punch: &'static str,
    /// The raw punch code.
    pub punch_code: u8,
    /// Work code, when the layout has one.
    pub workcode: Option<u32>,
}

impl From<&Attendance> for Punched {
    fn from(a: &Attendance) -> Self {
        Self {
            uid: a.uid.map(|u| u.0),
            user_id: a.user_id.as_ref().map(|id| id.as_str().to_owned()),
            status: status_name(a.status),
            status_code: a.status.to_byte(),
            punch: punch_name(a.punch),
            punch_code: a.punch.to_byte(),
            workcode: a.workcode,
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

/// One record from `attendance pull`.
#[derive(Debug, Clone, Serialize)]
pub struct RecordOut {
    /// The device (profile name or host).
    pub device: String,
    /// Device-local time.
    pub timestamp: String,
    /// The record.
    #[serde(flatten)]
    pub punch: Punched,
    /// With `--with-users`: `record`, `user_table` or `unresolved`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id_source: Option<&'static str>,
}

impl RecordOut {
    /// A record, with the source of its user ID if it was resolved.
    #[must_use]
    pub fn new(device: &str, a: &Attendance, source: Option<UserIdSource>) -> Self {
        Self {
            device: device.to_owned(),
            timestamp: device_time(a.timestamp),
            punch: Punched::from(a),
            user_id_source: source.map(|s| match s {
                UserIdSource::Record => "record",
                UserIdSource::UserTable => "user_table",
                UserIdSource::Unresolved => "unresolved",
            }),
        }
    }
}

/// One event from `live`.
#[derive(Debug, Clone, Serialize)]
pub struct EventOut {
    /// The device (profile name or host).
    pub device: String,
    /// `live`, or `backfill` for punches read from the log after a
    /// reconnect.
    pub source: &'static str,
    /// Host time of arrival (RFC 3339, UTC).
    pub received_at: String,
    /// Device clock in the event, device-local; `null` for raw events.
    pub device_time: Option<String>,
    /// The event.
    #[serde(flatten)]
    pub body: EventBody,
}

/// What an event holds.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventBody {
    /// A punch.
    Attendance(Punched),
    /// An event that was not decoded.
    Raw {
        /// The event flags.
        flags: u32,
        /// The payload, hex.
        payload_hex: String,
    },
}

impl EventOut {
    /// An event from device `device`.
    #[must_use]
    pub fn new(device: &str, e: &CapturedEvent) -> Self {
        Self {
            device: device.to_owned(),
            source: match e.source {
                Source::Live => "live",
                Source::Backfill => "backfill",
            },
            received_at: host_time(e.received_at),
            device_time: e.device_time.map(device_time),
            body: body(&e.event),
        }
    }
}

/// The body of an event.
#[must_use]
pub fn body(e: &LiveEvent) -> EventBody {
    match e {
        LiveEvent::Attendance(a) => EventBody::Attendance(Punched::from(a)),
        LiveEvent::Raw { flag, payload } => EventBody::Raw {
            flags: flag.bits(),
            payload_hex: hex::encode(payload),
        },
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
