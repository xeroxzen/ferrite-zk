//! `live --state FILE`: per device, the newest device time emitted and the
//! attendance records emitted at that time. With `--backfill`, a restarted
//! `live` resumes from it (`CaptureOptions::resume`), so punches stored
//! while it was not running are emitted once.
//!
//! A missing, unreadable or corrupt file is reported and replaced: losing
//! the state costs a catch-up, never the stream.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use zkteco_client::zkteco_proto::records::attendance::{AttendanceStatus, Punch};
use zkteco_client::{Attendance, Resume, Uid, UserId, DEDUP_CAPACITY};

/// File format version.
const VERSION: u32 = 1;

/// The state file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LiveState {
    v: u32,
    devices: BTreeMap<String, Mark>,
}

/// One device's watermark.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Mark {
    /// The newest device time emitted.
    device_time: NaiveDateTime,
    /// The attendance records emitted at exactly `device_time`, at most
    /// [`DEDUP_CAPACITY`], oldest first.
    delivered: Vec<Delivered>,
}

/// What identifies one emitted punch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Delivered {
    uid: Option<u16>,
    user_id: Option<String>,
    /// Verification method code.
    status_code: u8,
    /// Punch state code.
    punch_code: u8,
}

impl Delivered {
    fn of(a: &Attendance) -> Self {
        Self {
            uid: a.uid.map(|u| u.0),
            user_id: a.user_id.as_ref().map(|id| id.as_str().to_owned()),
            status_code: a.status.to_byte(),
            punch_code: a.punch.to_byte(),
        }
    }

    fn at(&self, timestamp: NaiveDateTime) -> Attendance {
        let mut record = Attendance::new(
            timestamp,
            AttendanceStatus::from_byte(self.status_code),
            Punch::from_byte(self.punch_code),
        );
        record.uid = self.uid.map(Uid);
        record.user_id = self.user_id.clone().map(UserId);
        record
    }
}

impl LiveState {
    /// No devices yet.
    pub(super) fn new() -> Self {
        Self {
            v: VERSION,
            devices: BTreeMap::new(),
        }
    }

    /// Read the file. Never fails: anything unusable is reported on stderr
    /// and treated as no state.
    pub(super) fn load(path: &Path) -> Self {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::info!(path = %path.display(), "no live state yet; starting fresh");
                return Self::new();
            }
            Err(e) => {
                eprintln!("warning: cannot read {}: {e}; starting fresh", path.display());
                return Self::new();
            }
        };
        match serde_json::from_str::<Self>(&text) {
            Ok(state) if state.v == VERSION => state,
            Ok(state) => {
                eprintln!(
                    "warning: {} has state version {}, expected {VERSION}; starting fresh",
                    path.display(),
                    state.v
                );
                Self::new()
            }
            Err(e) => {
                eprintln!("warning: {} is corrupt ({e}); starting fresh", path.display());
                Self::new()
            }
        }
    }

    /// Where `device`'s previous run stopped, if it emitted anything.
    pub(super) fn resume(&self, device: &str) -> Option<Resume> {
        let mark = self.devices.get(device)?;
        Some(Resume::new(
            mark.device_time,
            mark.delivered.iter().map(|d| d.at(mark.device_time)).collect(),
        ))
    }

    /// Note that `a` was emitted for `device`. `true` if the state changed.
    /// A record older than the watermark (the device clock was set back)
    /// leaves it alone.
    pub(super) fn record(&mut self, device: &str, a: &Attendance) -> bool {
        let entry = Delivered::of(a);
        match self.devices.get_mut(device) {
            Some(mark) if a.timestamp < mark.device_time => false,
            Some(mark) if a.timestamp == mark.device_time => {
                if mark.delivered.contains(&entry) {
                    return false;
                }
                if mark.delivered.len() >= DEDUP_CAPACITY {
                    mark.delivered.remove(0);
                }
                mark.delivered.push(entry);
                true
            }
            _ => {
                let mark = Mark {
                    device_time: a.timestamp,
                    delivered: vec![entry],
                };
                self.devices.insert(device.to_owned(), mark);
                true
            }
        }
    }

    /// Write the file atomically and durably.
    ///
    /// # Errors
    ///
    /// If it cannot be written; the previous file is then intact.
    pub(super) fn save(&self, path: &Path) -> Result<()> {
        let text = serde_json::to_string_pretty(self)? + "\n";
        crate::files::write_atomic(path, text.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn punch(user: u16, second: u32) -> Attendance {
        let mut record = Attendance::new(
            NaiveDate::from_ymd_opt(2026, 1, 1)
                .unwrap()
                .and_hms_opt(8, 0, second)
                .unwrap(),
            AttendanceStatus::Fingerprint,
            Punch::CheckIn,
        );
        record.uid = Some(Uid(user));
        record.user_id = Some(UserId(user.to_string()));
        record
    }

    #[test]
    fn keeps_the_newest_time_and_everything_at_it() {
        let mut s = LiveState::new();
        assert!(s.record("front", &punch(1, 0)));
        assert!(s.record("front", &punch(2, 5)));
        assert!(s.record("front", &punch(3, 5)));
        assert!(!s.record("front", &punch(3, 5)), "already noted");
        assert!(!s.record("front", &punch(4, 1)), "older than the watermark");
        let resume = s.resume("front").unwrap();
        assert_eq!(resume.since, punch(0, 5).timestamp);
        assert_eq!(resume.delivered, [punch(2, 5), punch(3, 5)]);
        assert!(s.resume("back").is_none());
    }

    #[test]
    fn round_trips_and_survives_bad_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("live.json");
        assert_eq!(LiveState::load(&path), LiveState::new(), "missing");
        let mut s = LiveState::new();
        s.record("front", &punch(1, 0));
        s.save(&path).unwrap();
        assert_eq!(LiveState::load(&path), s);
        for bad in [
            "{",
            "[]",
            r#"{"v":2,"devices":{}}"#,
            r#"{"v":1,"devices":{"x":{"device_time":"nope","delivered":[]}}}"#,
        ] {
            std::fs::write(&path, bad).unwrap();
            assert_eq!(LiveState::load(&path), LiveState::new(), "{bad}");
        }
    }
}
