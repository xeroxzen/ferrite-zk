//! User records.

use std::borrow::Cow;

use super::ids::{Uid, UserId};
use super::{decode_fixed, split_table, u16_at, u32_at, Batch};
use crate::error::{ProtoError, RecordKind, Result};
use crate::text::TextEncoding;

/// Bit 0 of the privilege byte: the user is disabled.
const DISABLED_BIT: u8 = 0x01;

/// User record layouts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UserFormat {
    /// 28-byte records used by older ("ZK6") firmware: numeric user ID,
    /// 8-byte name.
    #[doc = "Unverified against hardware."]
    Compact28,
    /// 72-byte records used by newer ("ZK8") firmware: string user ID,
    /// 24-byte name.
    #[doc = "Unverified against hardware."]
    Standard72,
    /// 120-byte records seen on NGTeco devices (NG-K4, NG-MB2), with first and
    /// last name stored separately. Read-only: two regions of the record
    /// are not understood, so this crate does not write it.
    #[doc = "Unverified against hardware."]
    NgTeco120,
}

impl UserFormat {
    /// Size of one record.
    #[must_use]
    pub const fn record_len(self) -> usize {
        match self {
            Self::Compact28 => 28,
            Self::Standard72 => 72,
            Self::NgTeco120 => 120,
        }
    }

    /// The format with this record size, if any.
    #[must_use]
    pub const fn from_record_len(len: usize) -> Option<Self> {
        match len {
            28 => Some(Self::Compact28),
            72 => Some(Self::Standard72),
            120 => Some(Self::NgTeco120),
            _ => None,
        }
    }

    /// Infer the format from a table's byte length and the user count the
    /// device reports (`CMD_GET_FREE_SIZES`).
    ///
    /// # Errors
    ///
    /// [`ProtoError::UnsupportedRecordFormat`] if the length is not an exact
    /// multiple of a known record size.
    pub fn detect(table_len: usize, user_count: usize) -> Result<Self> {
        let unsupported = |detail: String| ProtoError::UnsupportedRecordFormat {
            kind: RecordKind::User,
            detail,
        };
        if user_count == 0 {
            return Err(unsupported(format!("{table_len} bytes for 0 users")));
        }
        if !table_len.is_multiple_of(user_count) {
            return Err(unsupported(format!(
                "{table_len} bytes is not a whole number of records for {user_count} users"
            )));
        }
        let len = table_len / user_count;
        Self::from_record_len(len).ok_or_else(|| unsupported(format!("{len}-byte records")))
    }
}

/// A user's role.
///
/// The privilege byte holds the role in bits 1 to 3 and a "disabled" flag in
/// bit 0 (see [`User::disabled`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Privilege {
    /// Ordinary user (0).
    User,
    /// May enroll users (2).
    Enroller,
    /// Manager (6).
    Manager,
    /// Administrator (14).
    Admin,
    /// A value read from a device that is not one of the roles above, with the
    /// disabled bit removed. Kept so that nothing is lost; cannot be written.
    Unknown(u8),
}

impl Privilege {
    /// Interpret a raw privilege byte. The disabled bit is ignored.
    #[must_use]
    pub const fn from_byte(byte: u8) -> Self {
        match byte & !DISABLED_BIT {
            0 => Self::User,
            2 => Self::Enroller,
            6 => Self::Manager,
            14 => Self::Admin,
            other => Self::Unknown(other),
        }
    }

    /// The role bits, including those of [`Privilege::Unknown`].
    #[must_use]
    pub const fn bits(self) -> u8 {
        match self {
            Self::User => 0,
            Self::Enroller => 2,
            Self::Manager => 6,
            Self::Admin => 14,
            Self::Unknown(v) => v,
        }
    }

    /// The role bits for writing to a device.
    ///
    /// # Errors
    ///
    /// [`ProtoError::InvalidPrivilege`] for [`Privilege::Unknown`].
    pub const fn writable_bits(self) -> Result<u8> {
        match self {
            Self::Unknown(v) => Err(ProtoError::InvalidPrivilege(v)),
            known => Ok(known.bits()),
        }
    }
}

/// A user as stored on the device.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct User {
    /// Device-internal record index (1-based on most firmware).
    pub uid: Uid,
    /// The user ID shown on the device and printed on attendance records.
    /// Numeric in [`UserFormat::Compact28`].
    pub user_id: UserId,
    /// Display name exactly as stored; may be empty. See [`User::display_name`].
    pub name: String,
    /// Keypad password; may be empty.
    pub password: String,
    /// Role.
    pub privilege: Privilege,
    /// Whether the user is disabled (privilege bit 0).
    pub disabled: bool,
    /// Card number; 0 when none.
    pub card: u32,
    /// Group. Numeric (0 to 255) in [`UserFormat::Compact28`].
    pub group_id: String,
}

impl User {
    /// An ordinary, enabled user with no name, password, card or group;
    /// set the public fields for the rest.
    #[must_use]
    pub const fn new(uid: Uid, user_id: UserId) -> Self {
        Self {
            uid,
            user_id,
            name: String::new(),
            password: String::new(),
            privilege: Privilege::User,
            disabled: false,
            card: 0,
            group_id: String::new(),
        }
    }

    /// The name, or `NN-<user_id>` when the stored name is empty. This
    /// matches what pyzk returns as the name.
    #[must_use]
    pub fn display_name(&self) -> Cow<'_, str> {
        if self.name.is_empty() {
            Cow::Owned(format!("NN-{}", self.user_id))
        } else {
            Cow::Borrowed(&self.name)
        }
    }

    /// The raw privilege byte (role bits plus disabled bit).
    #[must_use]
    pub const fn privilege_byte(&self) -> u8 {
        self.privilege.bits() | if self.disabled { DISABLED_BIT } else { 0 }
    }

    fn writable_privilege_byte(&self) -> Result<u8> {
        Ok(self.privilege.writable_bits()? | if self.disabled { DISABLED_BIT } else { 0 })
    }
}

/// Decode a user table returned by a buffered read: a `u32` byte length
/// followed by fixed-size records. The format is inferred from the length
/// and `user_count` (see [`UserFormat::detect`]).
///
/// # Errors
///
/// Fails only if the length prefix is missing or the format cannot be
/// determined. Individual bad records go to [`Batch::rejected`].
pub fn decode_table(data: &[u8], user_count: usize, enc: TextEncoding) -> Result<Batch<User>> {
    let (announced, _, _) = split_table(data)?;
    let format = UserFormat::detect(announced, user_count)?;
    decode_table_as(data, format, enc)
}

/// Decode a user table with a known format.
///
/// # Errors
///
/// Fails only if the length prefix is missing.
pub fn decode_table_as(data: &[u8], format: UserFormat, enc: TextEncoding) -> Result<Batch<User>> {
    let (announced, area, trailing) = split_table(data)?;
    Ok(decode_fixed(
        announced,
        area,
        trailing,
        format.record_len(),
        |rec| decode_record(rec, format, enc),
    ))
}

/// Decode one record of exactly `format.record_len()` bytes.
///
/// # Errors
///
/// [`ProtoError::InvalidField`] if `rec` has the wrong length.
pub fn decode_record(rec: &[u8], format: UserFormat, enc: TextEncoding) -> Result<User> {
    super::check_len(rec, format.record_len())?;
    let uid = Uid(u16_at(rec, 0));
    let privilege = Privilege::from_byte(rec[2]);
    let disabled = rec[2] & DISABLED_BIT != 0;
    Ok(match format {
        // uid u16 | privilege u8 | password[5] | name[8] | card u32 | pad |
        // group u8 | time zone i16 | user_id u32
        UserFormat::Compact28 => User {
            uid,
            privilege,
            disabled,
            password: enc.decode_field(&rec[3..8]),
            name: enc.decode_field(&rec[8..16]),
            card: u32_at(rec, 16),
            group_id: rec[21].to_string(),
            user_id: UserId(u32_at(rec, 24).to_string()),
        },
        // uid u16 | privilege u8 | password[8] | name[24] | card u32 | flag u8 |
        // group[7] | pad | user_id[24]
        UserFormat::Standard72 => User {
            uid,
            privilege,
            disabled,
            password: enc.decode_field(&rec[3..11]),
            name: enc.decode_field(&rec[11..35]),
            card: u32_at(rec, 35),
            group_id: enc.decode_field(&rec[40..47]),
            user_id: UserId(enc.decode_field(&rec[48..72])),
        },
        // uid u16 | privilege u8 | password[8] | unknown[24] | first[24] |
        // last[24] | card u32 | unknown[9] | user_id[24]
        UserFormat::NgTeco120 => {
            let first = enc.decode_field(&rec[35..59]);
            let last = enc.decode_field(&rec[59..83]);
            let name = match (first.is_empty(), last.is_empty()) {
                (_, true) => first,
                (true, false) => last,
                (false, false) => format!("{first} {last}"),
            };
            User {
                uid,
                privilege,
                disabled,
                password: enc.decode_field(&rec[3..11]),
                name,
                card: u32_at(rec, 83),
                group_id: String::new(),
                user_id: UserId(enc.decode_field(&rec[96..120])),
            }
        }
    })
}

/// Encode one record, as sent with [`Command::UserWrq`](crate::Command::UserWrq)
/// and as found in user tables.
///
/// # Errors
///
/// Fails if a field does not fit or is invalid for the format, if the
/// privilege is [`Privilege::Unknown`], or for [`UserFormat::NgTeco120`].
pub fn encode_record(user: &User, format: UserFormat, enc: TextEncoding) -> Result<Vec<u8>> {
    write_record(user, format, enc, 0)
}

/// Encode a whole table: `u32` byte length followed by the records.
///
/// # Errors
///
/// As [`encode_record`].
pub fn encode_table(users: &[User], format: UserFormat, enc: TextEncoding) -> Result<Vec<u8>> {
    let mut body = Vec::with_capacity(users.len() * format.record_len());
    for u in users {
        body.extend(encode_record(u, format, enc)?);
    }
    let mut out = u32::try_from(body.len())
        .map_err(|_| ProtoError::Unsupported("user table larger than 4 GiB"))?
        .to_le_bytes()
        .to_vec();
    out.extend(body);
    Ok(out)
}

/// Shared writer. `flag72` is the byte after the card number in the 72-byte
/// layout, whose meaning is unknown: 0 in single-user writes, 1 in bulk saves.
pub(crate) fn write_record(
    user: &User,
    format: UserFormat,
    enc: TextEncoding,
    flag72: u8,
) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(format.record_len());
    out.extend_from_slice(&user.uid.0.to_le_bytes());
    out.push(user.writable_privilege_byte()?);
    match format {
        UserFormat::Compact28 => {
            enc.encode_field("password", &user.password, 5, &mut out)?;
            enc.encode_field("name", &user.name, 8, &mut out)?;
            out.extend_from_slice(&user.card.to_le_bytes());
            out.push(0);
            out.push(parse_number::<u8>("group_id", &user.group_id, true)?);
            out.extend_from_slice(&0i16.to_le_bytes());
            out.extend_from_slice(
                &parse_number::<u32>("user_id", user.user_id.as_str(), false)?.to_le_bytes(),
            );
        }
        UserFormat::Standard72 => {
            enc.encode_field("password", &user.password, 8, &mut out)?;
            enc.encode_field("name", &user.name, 24, &mut out)?;
            out.extend_from_slice(&user.card.to_le_bytes());
            out.push(flag72);
            enc.encode_field("group_id", &user.group_id, 7, &mut out)?;
            out.push(0);
            enc.encode_field("user_id", user.user_id.as_str(), 24, &mut out)?;
        }
        UserFormat::NgTeco120 => {
            return Err(ProtoError::Unsupported(
                "writing 120-byte user records is not supported",
            ));
        }
    }
    debug_assert_eq!(out.len(), format.record_len(), "encoded record length");
    Ok(out)
}

fn parse_number<T: std::str::FromStr + Default>(
    field: &'static str,
    s: &str,
    empty_is_zero: bool,
) -> Result<T> {
    if s.is_empty() && empty_is_zero {
        return Ok(T::default());
    }
    s.parse().map_err(|_| ProtoError::InvalidField {
        field,
        reason: format!("{s:?} is not a number in range for the 28-byte layout"),
    })
}

/// The next unused `uid` and a matching unused `user_id` for a new user.
///
/// `uid` is one past the highest existing uid. `user_id` is that number as a
/// string, incremented until no existing user has it.
///
/// Returns `None` when the uid space is exhausted.
#[must_use]
pub fn next_free_ids(users: &[User]) -> Option<(Uid, UserId)> {
    let uid = users.iter().map(|u| u.uid.0).max().unwrap_or(0).checked_add(1)?;
    let taken: std::collections::HashSet<&str> = users.iter().map(|u| u.user_id.as_str()).collect();
    // At most users.len() + 1 candidates can be tried before one is free.
    (u64::from(uid)..=u64::from(uid) + users.len() as u64)
        .map(|n| n.to_string())
        .find(|id| !taken.contains(id.as_str()))
        .map(|id| (Uid(uid), UserId(id)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user() -> User {
        User {
            uid: Uid(7),
            user_id: "1234".into(),
            name: "Ada".into(),
            password: "99".into(),
            privilege: Privilege::Admin,
            disabled: false,
            card: 0xDEAD_BEEF,
            group_id: "1".into(),
        }
    }

    #[test]
    fn round_trip_both_writable_formats() {
        let enc = TextEncoding::UTF8;
        for f in [UserFormat::Compact28, UserFormat::Standard72] {
            let rec = encode_record(&user(), f, enc).unwrap();
            assert_eq!(rec.len(), f.record_len());
            assert_eq!(decode_record(&rec, f, enc).unwrap(), user());
        }
    }

    #[test]
    fn compact_layout_offsets() {
        let rec = encode_record(&user(), UserFormat::Compact28, TextEncoding::UTF8).unwrap();
        assert_eq!(&rec[0..3], &[7, 0, 14]);
        assert_eq!(&rec[3..8], b"99\0\0\0");
        assert_eq!(&rec[8..16], b"Ada\0\0\0\0\0");
        assert_eq!(&rec[16..20], &0xDEAD_BEEFu32.to_le_bytes());
        assert_eq!(rec[21], 1);
        assert_eq!(&rec[24..28], &1234u32.to_le_bytes());
    }

    #[test]
    fn privilege_handling() {
        assert_eq!(Privilege::from_byte(15), Privilege::Admin);
        assert_eq!(Privilege::from_byte(4), Privilege::Unknown(4));
        assert_eq!(Privilege::from_byte(0x11), Privilege::Unknown(0x10));
        let mut u = user();
        u.privilege = Privilege::Unknown(4);
        assert_eq!(
            encode_record(&u, UserFormat::Standard72, TextEncoding::UTF8),
            Err(ProtoError::InvalidPrivilege(4))
        );
        u.privilege = Privilege::Manager;
        u.disabled = true;
        let rec = encode_record(&u, UserFormat::Standard72, TextEncoding::UTF8).unwrap();
        assert_eq!(rec[2], 7);
    }

    #[test]
    fn compact_rejects_non_numeric_ids_and_long_names() {
        let mut u = user();
        u.user_id = "A1".into();
        assert!(matches!(
            encode_record(&u, UserFormat::Compact28, TextEncoding::UTF8),
            Err(ProtoError::InvalidField { field: "user_id", .. })
        ));
        let mut u = user();
        u.name = "123456789".into();
        assert!(matches!(
            encode_record(&u, UserFormat::Compact28, TextEncoding::UTF8),
            Err(ProtoError::FieldTooLong { field: "name", .. })
        ));
    }

    #[test]
    fn detect_formats() {
        assert_eq!(UserFormat::detect(144, 2).unwrap(), UserFormat::Standard72);
        assert_eq!(UserFormat::detect(56, 2).unwrap(), UserFormat::Compact28);
        assert_eq!(UserFormat::detect(360, 3).unwrap(), UserFormat::NgTeco120);
        assert!(UserFormat::detect(145, 2).is_err());
        assert!(UserFormat::detect(100, 2).is_err());
        assert!(UserFormat::detect(0, 0).is_err());
    }

    #[test]
    fn table_with_truncated_and_trailing_bytes() {
        let enc = TextEncoding::UTF8;
        let mut t = encode_table(&[user(), user()], UserFormat::Standard72, enc).unwrap();
        t.truncate(t.len() - 10);
        let b = decode_table_as(&t, UserFormat::Standard72, enc).unwrap();
        assert_eq!(b.records.len(), 1);
        assert_eq!(b.rejected.len(), 2, "partial record and short read");
        assert_eq!(b.rejected[0].offset, 72);

        let mut t = encode_table(&[user()], UserFormat::Standard72, enc).unwrap();
        t.extend_from_slice(&[1, 2, 3]);
        let b = decode_table_as(&t, UserFormat::Standard72, enc).unwrap();
        assert_eq!(b.records.len(), 1);
        assert!(matches!(b.rejected[0].reason, ProtoError::Overrun { .. }));
    }

    #[test]
    fn ngteco_names() {
        let mut rec = vec![0u8; 120];
        rec[0] = 3;
        rec[35..38].copy_from_slice(b"Ada");
        rec[59..67].copy_from_slice(b"Lovelace");
        rec[96..99].copy_from_slice(b"A42");
        let u = decode_record(&rec, UserFormat::NgTeco120, TextEncoding::UTF8).unwrap();
        assert_eq!(u.name, "Ada Lovelace");
        assert_eq!(u.user_id, "A42");
        assert!(encode_record(&u, UserFormat::NgTeco120, TextEncoding::UTF8).is_err());
    }

    #[test]
    fn next_ids() {
        assert_eq!(next_free_ids(&[]), Some((Uid(1), "1".into())));
        let mut a = user();
        a.uid = Uid(2);
        a.user_id = "3".into();
        let mut b = user();
        b.uid = Uid(1);
        b.user_id = "4".into();
        assert_eq!(next_free_ids(&[a, b]), Some((Uid(3), "5".into())));
        let mut full = user();
        full.uid = Uid(u16::MAX);
        assert_eq!(next_free_ids(&[full]), None);
    }
}
