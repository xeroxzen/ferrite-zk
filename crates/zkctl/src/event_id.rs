//! The `id` of an event: a deterministic identifier, so that the same punch
//! read by `attendance pull`, delivered live, or backfilled gets the same
//! `id`, and consumers can deduplicate on it (DECISIONS.md D36).
//!
//! `id` is the first 16 bytes of SHA-256, in lowercase hex, over a sequence
//! of fields, each prefixed by its length as an 8-byte big-endian integer:
//!
//! - attendance: `zkctl-event-v1`, `attendance`, device, device time
//!   (`YYYY-MM-DDTHH:MM:SS`), who (`user_id:<user ID>`, else `uid:<uid>`,
//!   else empty), status code (1 byte), punch code (1 byte);
//! - raw: `zkctl-event-v1`, `raw`, device, flags (4 bytes, big-endian),
//!   payload.
//!
//! The person is the user ID when there is one, not the uid as well: live
//! events on some layouts carry only the user ID, while the log carries
//! both, and the same punch must get the same `id`.

use sha2::{Digest, Sha256};
use zkteco_client::Attendance;

use crate::output::device_time;

/// Domain separation, and the version of this encoding.
const DOMAIN: &[u8] = b"zkctl-event-v1";

/// The `id` of an attendance record from `device`.
#[must_use]
pub fn attendance(device: &str, a: &Attendance) -> String {
    let who = match (&a.user_id, a.uid) {
        (Some(id), _) => format!("user_id:{}", id.as_str()),
        (None, Some(uid)) => format!("uid:{}", uid.0),
        (None, None) => String::new(),
    };
    digest(&[
        DOMAIN,
        b"attendance",
        device.as_bytes(),
        device_time(a.timestamp).as_bytes(),
        who.as_bytes(),
        &[a.status.to_byte()],
        &[a.punch.to_byte()],
    ])
}

/// The `id` of an undecoded event from `device`.
#[must_use]
pub fn raw(device: &str, flags: u32, payload: &[u8]) -> String {
    digest(&[DOMAIN, b"raw", device.as_bytes(), &flags.to_be_bytes(), payload])
}

fn digest(fields: &[&[u8]]) -> String {
    let mut hash = Sha256::new();
    for field in fields {
        hash.update((field.len() as u64).to_be_bytes());
        hash.update(field);
    }
    hex::encode(&hash.finalize()[..16])
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use zkteco_client::zkteco_proto::records::attendance::{AttendanceStatus, Punch};
    use zkteco_client::{Uid, UserId};

    fn punch(uid: Option<u16>, user_id: Option<&str>) -> Attendance {
        Attendance {
            uid: uid.map(Uid),
            user_id: user_id.map(|s| UserId(s.into())),
            timestamp: NaiveDate::from_ymd_opt(2026, 1, 2)
                .unwrap()
                .and_hms_opt(3, 4, 5)
                .unwrap(),
            status: AttendanceStatus::Fingerprint,
            punch: Punch::CheckIn,
            workcode: None,
        }
    }

    /// Pinned: changing the encoding changes every published `id`, which
    /// needs a new schema version.
    #[test]
    fn encoding_is_stable() {
        assert_eq!(
            attendance("front", &punch(Some(7), Some("1001"))),
            "28ae83f83bc20808c3d10e07af928880"
        );
        assert_eq!(
            raw("front", 0x4, &[55, 0, 0, 0]),
            "638ab863eac15e19f3c85e42cd6dd4ce"
        );
    }

    #[test]
    fn the_person_is_the_user_id_when_known() {
        let both = attendance("front", &punch(Some(7), Some("1001")));
        assert_eq!(both, attendance("front", &punch(None, Some("1001"))));
        assert_ne!(both, attendance("front", &punch(Some(7), None)));
        assert_ne!(
            attendance("front", &punch(Some(7), None)),
            attendance("front", &punch(None, None))
        );
    }

    #[test]
    fn every_field_counts() {
        let base = punch(None, Some("1"));
        let id = attendance("front", &base);
        assert_eq!(id.len(), 32);
        assert!(id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        assert_ne!(id, attendance("back", &base));
        let mut later = base.clone();
        later.timestamp += chrono::Duration::seconds(1);
        assert_ne!(id, attendance("front", &later));
        let mut card = base.clone();
        card.status = AttendanceStatus::Card;
        assert_ne!(id, attendance("front", &card));
        let mut out = base.clone();
        out.punch = Punch::CheckOut;
        assert_ne!(id, attendance("front", &out));
        // Length prefixes keep field boundaries: "ab"+"c" differs from "a"+"bc".
        assert_ne!(raw("ab", 0, b"c"), raw("a", 0, b"bc"));
    }
}
