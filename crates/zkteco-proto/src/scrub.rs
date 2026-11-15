//! Remove personal data and device identity from table images and option
//! replies, in place and without changing any length, so a capture keeps
//! exercising the same layouts (the rules in `tests/fixtures/captures/`).
//!
//! What is replaced:
//!
//! - user records: name, password and card are cleared or replaced, and the
//!   user ID is replaced by a synthetic number;
//! - attendance records: the user ID is replaced (consistently with the user
//!   table, so the two still join) and the timestamp becomes a synthetic one;
//! - regions whose meaning is unknown (in the 120-byte user layout and the
//!   40- and 49-byte attendance layouts) are zeroed, because they may hold
//!   personal data;
//! - option values that identify the device or its network (serial number,
//!   MAC and IP settings).
//!
//! The device-internal `uid`, privileges, status and punch codes are kept:
//! they identify no one and are part of the layout being verified.

use std::collections::HashMap;

use chrono::{NaiveDate, NaiveDateTime, TimeDelta};

use crate::records::attendance::{AttendanceFormat, PREFIX_49};
use crate::records::user::UserFormat;
use crate::time;

/// Option keys whose values are scrubbed.
pub const IDENTIFYING_OPTIONS: &[&str] = &["~SerialNumber", "MAC", "IPAddress", "GATEIPAddress", "NetMask"];

/// Replaces personal data, mapping each original user ID to the same
/// synthetic ID across every table it scrubs.
#[derive(Debug, Default)]
pub struct Scrubber {
    ids: HashMap<Vec<u8>, u32>,
}

impl Scrubber {
    /// A scrubber with no IDs mapped yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The synthetic ID for an original user ID (its raw bytes), assigned
    /// in order of first appearance from 1.
    fn id_for(&mut self, original: &[u8]) -> u32 {
        let end = original.iter().position(|&b| b == 0).unwrap_or(original.len());
        let next = u32::try_from(self.ids.len() + 1).unwrap_or(u32::MAX);
        *self.ids.entry(original[..end].to_vec()).or_insert(next)
    }

    /// Scrub a user table image (`u32` length, then records). A trailing
    /// partial record is zeroed.
    pub fn user_table(&mut self, table: &mut [u8], format: UserFormat) {
        for_each_record(table, format.record_len(), |rec| self.user_record(rec, format));
    }

    /// Scrub an attendance table image (`u32` length, then records). A
    /// trailing partial record is zeroed.
    pub fn attendance_table(&mut self, table: &mut [u8], format: AttendanceFormat) {
        let mut index = 0u32;
        for_each_record(table, format.record_len(), |rec| {
            self.attendance_record(rec, format, index);
            index = index.saturating_add(1);
        });
    }

    fn user_record(&mut self, rec: &mut [u8], format: UserFormat) {
        let uid = u16::from_le_bytes([rec[0], rec[1]]);
        match format {
            // password 3..8, name 8..16, card 16..20, user_id u32 24..28
            UserFormat::Compact28 => {
                rec[3..8].fill(0);
                put_text(&mut rec[8..16], &format!("User{uid}"));
                rec[16..20].fill(0);
                let id = self.id_for(&rec[24..28]);
                rec[24..28].copy_from_slice(&id.to_le_bytes());
            }
            // password 3..11, name 11..35, card 35..39, user_id 48..72
            UserFormat::Standard72 => {
                rec[3..11].fill(0);
                put_text(&mut rec[11..35], &format!("User{uid}"));
                rec[35..39].fill(0);
                let id = self.id_for(&rec[48..72]);
                put_text(&mut rec[48..72], &id.to_string());
            }
            // password 3..11, unknown 11..35, first 35..59, last 59..83,
            // card 83..87, unknown 87..96, user_id 96..120
            UserFormat::NgTeco120 => {
                rec[3..35].fill(0);
                put_text(&mut rec[35..59], &format!("User{uid}"));
                rec[59..96].fill(0);
                let id = self.id_for(&rec[96..120]);
                put_text(&mut rec[96..120], &id.to_string());
            }
        }
    }

    fn attendance_record(&mut self, rec: &mut [u8], format: AttendanceFormat, index: u32) {
        let stamp = synthetic_time(index);
        match format {
            // uid 0..2, status 2, time 3..7, punch 7
            AttendanceFormat::Short8 => rec[3..7].copy_from_slice(&stamp),
            // user_id u32 0..4, time 4..8, status 8, punch 9, reserved, workcode
            AttendanceFormat::Medium16 => {
                let id = self.id_for(&rec[0..4]);
                rec[0..4].copy_from_slice(&id.to_le_bytes());
                rec[4..8].copy_from_slice(&stamp);
            }
            AttendanceFormat::Long40 => self.long40_body(rec, stamp),
            AttendanceFormat::Long49 if rec[..9] == PREFIX_49 => self.long40_body(&mut rec[9..], stamp),
            // Unprefixed: the 40-byte body's fields at 0, then bytes of
            // unknown meaning.
            AttendanceFormat::Long49 => {
                self.long40_body(&mut rec[..40], stamp);
                rec[32..].fill(0);
            }
        }
    }

    // uid 0..2, user_id 2..26, status 26, time 27..31, punch 31, reserved 32..40
    fn long40_body(&mut self, body: &mut [u8], stamp: [u8; 4]) {
        let id = self.id_for(&body[2..26]);
        put_text(&mut body[2..26], &id.to_string());
        body[27..31].copy_from_slice(&stamp);
        body[32..40].fill(0);
    }
}

/// Scrub an option reply payload (`key=value`, NUL-terminated) in place if
/// its key is in [`IDENTIFYING_OPTIONS`]: every letter or digit of the value
/// becomes `0`, and separators are kept. Returns whether it changed
/// anything.
pub fn option_reply(payload: &mut [u8]) -> bool {
    let Some(eq) = payload.iter().position(|&b| b == b'=') else {
        return false;
    };
    let key = &payload[..eq];
    if !IDENTIFYING_OPTIONS.iter().any(|k| k.as_bytes() == key) {
        return false;
    }
    let mut changed = false;
    for b in &mut payload[eq + 1..] {
        if *b == 0 {
            break;
        }
        if b.is_ascii_alphanumeric() || !b.is_ascii() {
            *b = b'0';
            changed = true;
        }
    }
    changed
}

/// Apply `f` to each whole record after the `u32` length prefix; zero a
/// trailing partial record. The record count bounds the loop.
fn for_each_record(table: &mut [u8], len: usize, mut f: impl FnMut(&mut [u8])) {
    let Some(body) = table.get_mut(4..) else {
        return;
    };
    let mut records = body.chunks_exact_mut(len);
    for rec in &mut records {
        f(rec);
    }
    records.into_remainder().fill(0);
}

/// Write ASCII `text` into a fixed field, NUL-padded, truncated to fit.
fn put_text(field: &mut [u8], text: &str) {
    field.fill(0);
    let n = text.len().min(field.len());
    field[..n].copy_from_slice(&text.as_bytes()[..n]);
}

/// A packed timestamp `index` minutes after 2000-01-01 00:00.
fn synthetic_time(index: u32) -> [u8; 4] {
    let base: NaiveDateTime = NaiveDate::from_ymd_opt(2000, 1, 1)
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .unwrap_or_default();
    // Wrap within ten years so the value always encodes.
    let t = base + TimeDelta::minutes(i64::from(index % (10 * 365 * 24 * 60)));
    time::encode_packed(t).unwrap_or(0).to_le_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::records::attendance::{self, Attendance, AttendanceStatus, Punch};
    use crate::records::user::{self, Privilege, User};
    use crate::{TextEncoding, Uid, UserId};

    fn user(uid: u16, id: &str) -> User {
        User {
            uid: Uid(uid),
            privilege: Privilege::Admin,
            disabled: false,
            password: "1234".into(),
            name: "Jane D".into(),
            card: 123_456,
            group_id: "1".into(),
            user_id: UserId(id.into()),
        }
    }

    #[test]
    fn user_table_keeps_layout_and_replaces_personal_fields() {
        let enc = TextEncoding::UTF8;
        for (format, a, b) in [
            (UserFormat::Standard72, "EMP-77", "EMP-78"),
            (UserFormat::Compact28, "77", "78"),
        ] {
            let mut table = user::encode_table(&[user(1, a), user(2, b)], format, enc).unwrap();
            let len = table.len();
            Scrubber::new().user_table(&mut table, format);
            assert_eq!(table.len(), len);
            let batch = user::decode_table_as(&table, format, enc).unwrap();
            assert_eq!(batch.rejected, Vec::new());
            let got: Vec<_> = batch
                .records
                .iter()
                .map(|u| (u.uid, u.user_id.as_str(), u.card))
                .collect();
            assert_eq!(got, [(Uid(1), "1", 0), (Uid(2), "2", 0)]);
            assert!(batch
                .records
                .iter()
                .all(|u| u.password.is_empty() && u.name.starts_with("User")));
            assert!(batch.records.iter().all(|u| u.privilege == Privilege::Admin));
        }
    }

    #[test]
    fn attendance_ids_join_with_users_and_times_are_synthetic() {
        let enc = TextEncoding::UTF8;
        let mut scrubber = Scrubber::new();
        let mut users = user::encode_table(&[user(1, "EMP-77")], UserFormat::Standard72, enc).unwrap();
        scrubber.user_table(&mut users, UserFormat::Standard72);
        let stamp = NaiveDate::from_ymd_opt(2026, 3, 4)
            .unwrap()
            .and_hms_opt(8, 1, 2)
            .unwrap();
        let punch = Attendance {
            uid: Some(Uid(1)),
            user_id: Some(UserId("EMP-77".into())),
            timestamp: stamp,
            status: AttendanceStatus::Fingerprint,
            punch: Punch::CheckIn,
            workcode: None,
        };
        for format in [AttendanceFormat::Long40, AttendanceFormat::Long49] {
            let mut table = attendance::encode_table(&[punch.clone(), punch.clone()], format, enc).unwrap();
            scrubber.attendance_table(&mut table, format);
            let batch = attendance::decode_table_as(&table, format, enc).unwrap();
            assert_eq!(batch.rejected, Vec::new());
            for (i, a) in batch.records.iter().enumerate() {
                assert_eq!(a.user_id.as_deref(), Some("1"));
                assert_eq!(a.uid, Some(Uid(1)));
                assert_eq!(
                    a.timestamp.and_utc().timestamp() - base_secs(),
                    60 * i64::try_from(i).unwrap()
                );
            }
        }
    }

    fn base_secs() -> i64 {
        NaiveDate::from_ymd_opt(2000, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp()
    }

    #[test]
    fn partial_record_is_zeroed() {
        let mut table = vec![0, 0, 0, 0, 1, 2, 3];
        Scrubber::new().attendance_table(&mut table, AttendanceFormat::Short8);
        assert_eq!(table, [0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn options() {
        let mut serial = b"~SerialNumber=ABC12-34\0".to_vec();
        assert!(option_reply(&mut serial));
        assert_eq!(serial, b"~SerialNumber=00000-00\0");
        let mut mac = b"MAC=00:17:61:aa:bb:cc\0".to_vec();
        assert!(option_reply(&mut mac));
        assert_eq!(mac, b"MAC=00:00:00:00:00:00\0");
        let mut platform = b"~Platform=ZMM220_TFT\0".to_vec();
        assert!(!option_reply(&mut platform));
        assert_eq!(platform, b"~Platform=ZMM220_TFT\0");
    }
}
