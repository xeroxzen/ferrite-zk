//! Recently delivered attendance events, so a backfilled record and a live
//! event for the same punch are delivered once.

use std::collections::{HashSet, VecDeque};

use chrono::NaiveDateTime;
use zkteco_proto::records::attendance::{Attendance, AttendanceStatus, Punch};

/// Keys remembered. Older keys are forgotten first; a duplicate older than
/// this many deliveries is delivered again.
pub const DEDUP_CAPACITY: usize = 4096;

/// What identifies one punch: the person, the device time, and how and
/// why they punched.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Key {
    who: String,
    when: NaiveDateTime,
    status: AttendanceStatus,
    punch: Punch,
}

impl Key {
    /// The key of a record, keyed by user ID, or by uid when the record has
    /// no user ID. `None` if it has neither.
    pub(crate) fn of(a: &Attendance) -> Option<Self> {
        let who = match (&a.user_id, a.uid) {
            (Some(id), _) => id.as_str().to_owned(),
            (None, Some(uid)) => format!("uid:{}", uid.0),
            (None, None) => return None,
        };
        Some(Self {
            who,
            when: a.timestamp,
            status: a.status,
            punch: a.punch,
        })
    }
}

/// A bounded set of recently seen keys.
#[derive(Debug, Default)]
pub(crate) struct Dedup {
    keys: HashSet<Key>,
    order: VecDeque<Key>,
}

impl Dedup {
    /// Remember `key`; `false` if it was already remembered.
    pub(crate) fn insert(&mut self, key: Key) -> bool {
        if !self.keys.insert(key.clone()) {
            return false;
        }
        self.order.push_back(key);
        if self.order.len() > DEDUP_CAPACITY {
            if let Some(old) = self.order.pop_front() {
                self.keys.remove(&old);
            }
        }
        debug_assert_eq!(self.keys.len(), self.order.len());
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use zkteco_proto::{Uid, UserId};

    fn punch(user_id: Option<&str>, uid: Option<u16>, minute: u32) -> Attendance {
        Attendance {
            uid: uid.map(Uid),
            user_id: user_id.map(|s| UserId(s.into())),
            timestamp: NaiveDate::from_ymd_opt(2026, 1, 1)
                .unwrap()
                .and_hms_opt(8, minute, 0)
                .unwrap(),
            status: AttendanceStatus::Fingerprint,
            punch: Punch::CheckIn,
            workcode: None,
        }
    }

    #[test]
    fn user_id_wins_over_uid() {
        let a = Key::of(&punch(Some("7"), Some(3), 0)).unwrap();
        let b = Key::of(&punch(Some("7"), None, 0)).unwrap();
        assert_eq!(a, b);
        assert!(Key::of(&punch(None, None, 0)).is_none());
    }

    #[test]
    fn remembers_up_to_capacity() {
        let mut d = Dedup::default();
        let first = Key::of(&punch(Some("0"), None, 0)).unwrap();
        assert!(d.insert(first.clone()));
        assert!(!d.insert(first.clone()));
        for i in 1..=DEDUP_CAPACITY {
            let k = Key::of(&punch(Some(&i.to_string()), None, 0)).unwrap();
            assert!(d.insert(k));
        }
        assert!(d.insert(first), "forgotten once the capacity is exceeded");
    }
}
