//! The `attendance pull --state` cursor: the last record emitted per
//! device, so the next run emits only records after it.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use zkteco_client::Attendance;

use crate::output;

/// The state file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct StateFile {
    /// Format version.
    pub(super) version: u32,
    /// Cursor per device name.
    pub(super) devices: BTreeMap<String, Cursor>,
}

/// The last record emitted: enough to find it again in the log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Cursor {
    uid: Option<u16>,
    user_id: Option<String>,
    timestamp: String,
    status_code: u8,
    punch_code: u8,
}

impl Cursor {
    pub(super) fn of(a: &Attendance) -> Self {
        Self {
            uid: a.uid.map(|u| u.0),
            user_id: a.user_id.as_ref().map(|id| id.as_str().to_owned()),
            timestamp: output::device_time(a.timestamp),
            status_code: a.status.to_byte(),
            punch_code: a.punch.to_byte(),
        }
    }

    fn matches(&self, a: &Attendance) -> bool {
        let other = Self::of(a);
        let same_person = match (&self.user_id, &other.user_id) {
            (Some(x), Some(y)) => x == y,
            _ => self.uid.is_some() && self.uid == other.uid,
        };
        same_person
            && self.timestamp == other.timestamp
            && self.status_code == other.status_code
            && self.punch_code == other.punch_code
    }
}

/// Indices of the records after `cursor`: those after its last match in
/// log order. If it is not in the log (cleared or rotated), the records
/// later than its time, with a warning.
pub(super) fn after(records: &[Attendance], cursor: Option<&Cursor>, device: &str) -> Vec<usize> {
    let Some(cursor) = cursor else {
        return (0..records.len()).collect();
    };
    if let Some(pos) = records.iter().rposition(|a| cursor.matches(a)) {
        return (pos + 1..records.len()).collect();
    }
    tracing::warn!(
        device,
        "the last exported record is no longer in the log (cleared?); exporting records after its time"
    );
    records
        .iter()
        .enumerate()
        .filter(|(_, a)| output::device_time(a.timestamp) > cursor.timestamp)
        .map(|(i, _)| i)
        .collect()
}

/// Load the state file; missing is empty.
pub(super) fn load(path: &Path) -> Result<StateFile> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(StateFile {
            version: 1,
            devices: BTreeMap::new(),
        }),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Write the state file atomically.
pub(super) fn save(path: &Path, state: &StateFile) -> Result<()> {
    crate::files::write_atomic(path, (serde_json::to_string_pretty(state)? + "\n").as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use zkteco_client::{Uid, UserId};
    use zkteco_proto::records::attendance::{AttendanceStatus, Punch};

    fn rec(id: &str, minute: u32) -> Attendance {
        Attendance {
            uid: Some(Uid(1)),
            user_id: Some(UserId(id.into())),
            timestamp: NaiveDate::from_ymd_opt(2026, 1, 1)
                .unwrap()
                .and_hms_opt(8, minute, 0)
                .unwrap(),
            status: AttendanceStatus::Card,
            punch: Punch::CheckIn,
            workcode: None,
        }
    }

    #[test]
    fn resumes_after_the_last_match() {
        let log = vec![rec("1", 0), rec("2", 1), rec("3", 1), rec("4", 2)];
        assert_eq!(after(&log, None, "d"), [0, 1, 2, 3]);
        assert_eq!(after(&log, Some(&Cursor::of(&log[1])), "d"), [2, 3]);
        assert_eq!(after(&log, Some(&Cursor::of(&log[3])), "d"), Vec::<usize>::new());
    }

    #[test]
    fn falls_back_to_time_when_the_record_is_gone() {
        let log = vec![rec("5", 0), rec("6", 3)];
        let gone = Cursor::of(&rec("9", 1));
        assert_eq!(after(&log, Some(&gone), "d"), [1]);
    }
}
