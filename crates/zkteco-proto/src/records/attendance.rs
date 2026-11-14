//! Attendance log records.

use chrono::NaiveDateTime;

use super::{decode_fixed, split_table, u16_at, u32_at, Batch};
use crate::error::{ProtoError, RecordKind, Result};
use crate::text::TextEncoding;
use crate::time;

/// The 9-byte marker that precedes the 40-byte body in one of the two
/// known 49-byte layouts.
pub const PREFIX_49: [u8; 9] = [0xff, b'2', b'5', b'5', 0, 0, 0, 0, 0];

/// Attendance record layouts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AttendanceFormat {
    /// 8 bytes: uid, status, time, punch. Carries no user ID string; join on
    /// uid with the user table.
    #[doc = "Unverified against hardware."]
    Short8,
    /// 16 bytes: numeric user ID, time, status, punch, work code.
    #[doc = "Unverified against hardware."]
    Medium16,
    /// 40 bytes: uid, user ID string, status, time, punch.
    #[doc = "Unverified against hardware."]
    Long40,
    /// 49 bytes, in one of two layouts told apart per record:
    ///
    /// - [`PREFIX_49`] followed by the 40-byte body (pyzk's handling;
    ///   reported for MB560-VL, pyzk issue #174);
    /// - the 40-byte body's fields at offset 0 followed by 17 bytes of
    ///   unknown meaning (reported for MB40-VL, unmerged pyzk PR #247).
    ///
    /// Encoding always produces the prefixed form.
    #[doc = "Unverified against hardware."]
    Long49,
}

impl AttendanceFormat {
    /// Size of one record.
    #[must_use]
    pub const fn record_len(self) -> usize {
        match self {
            Self::Short8 => 8,
            Self::Medium16 => 16,
            Self::Long40 => 40,
            Self::Long49 => 49,
        }
    }

    /// The format with this record size, if any.
    #[must_use]
    pub const fn from_record_len(len: usize) -> Option<Self> {
        match len {
            8 => Some(Self::Short8),
            16 => Some(Self::Medium16),
            40 => Some(Self::Long40),
            49 => Some(Self::Long49),
            _ => None,
        }
    }

    /// Infer the format from a table's byte length and the record count the
    /// device reports (`CMD_GET_FREE_SIZES`).
    ///
    /// # Errors
    ///
    /// [`ProtoError::UnsupportedRecordFormat`] if the length is not an exact
    /// multiple of a known record size.
    pub fn detect(table_len: usize, record_count: usize) -> Result<Self> {
        let unsupported = |detail: String| ProtoError::UnsupportedRecordFormat {
            kind: RecordKind::Attendance,
            detail,
        };
        if record_count == 0 || !table_len.is_multiple_of(record_count) {
            return Err(unsupported(format!(
                "{table_len} bytes is not a whole number of records for {record_count} records"
            )));
        }
        let len = table_len / record_count;
        Self::from_record_len(len).ok_or_else(|| unsupported(format!("{len}-byte records")))
    }
}

/// How the user was verified. Values are firmware-defined; the named
/// variants are the common meanings.
#[doc = "Unverified against hardware."]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AttendanceStatus {
    /// 0: keypad password.
    Password,
    /// 1: fingerprint.
    Fingerprint,
    /// 2: card.
    Card,
    /// 15: face.
    Face,
    /// Any other value.
    Unknown(u8),
}

impl AttendanceStatus {
    /// Interpret a raw value.
    #[must_use]
    pub const fn from_byte(b: u8) -> Self {
        match b {
            0 => Self::Password,
            1 => Self::Fingerprint,
            2 => Self::Card,
            15 => Self::Face,
            other => Self::Unknown(other),
        }
    }

    /// The raw value.
    #[must_use]
    pub const fn to_byte(self) -> u8 {
        match self {
            Self::Password => 0,
            Self::Fingerprint => 1,
            Self::Card => 2,
            Self::Face => 15,
            Self::Unknown(v) => v,
        }
    }
}

/// The attendance state the user selected (or the device assigned).
#[doc = "Unverified against hardware."]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Punch {
    /// 0.
    CheckIn,
    /// 1.
    CheckOut,
    /// 2.
    BreakOut,
    /// 3.
    BreakIn,
    /// 4.
    OvertimeIn,
    /// 5.
    OvertimeOut,
    /// Any other value.
    Unknown(u8),
}

impl Punch {
    /// Interpret a raw value.
    #[must_use]
    pub const fn from_byte(b: u8) -> Self {
        match b {
            0 => Self::CheckIn,
            1 => Self::CheckOut,
            2 => Self::BreakOut,
            3 => Self::BreakIn,
            4 => Self::OvertimeIn,
            5 => Self::OvertimeOut,
            other => Self::Unknown(other),
        }
    }

    /// The raw value.
    #[must_use]
    pub const fn to_byte(self) -> u8 {
        match self {
            Self::CheckIn => 0,
            Self::CheckOut => 1,
            Self::BreakOut => 2,
            Self::BreakIn => 3,
            Self::OvertimeIn => 4,
            Self::OvertimeOut => 5,
            Self::Unknown(v) => v,
        }
    }
}

/// One attendance record, from the log or from a real-time event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attendance {
    /// Device-internal user index, when the layout carries it.
    pub uid: Option<u16>,
    /// User ID string, when the layout carries it. The 8-byte layout does not;
    /// join on `uid` with the user table.
    pub user_id: Option<String>,
    /// Device-local time (no time zone).
    pub timestamp: NaiveDateTime,
    /// Verification method.
    pub status: AttendanceStatus,
    /// Attendance state.
    pub punch: Punch,
    /// Work code, in the 16-byte layout.
    pub workcode: Option<u32>,
}

/// A batch of attendance records with per-record rejections.
pub type AttendanceBatch = Batch<Attendance>;

/// Decode an attendance table returned by a buffered read: a `u32` byte
/// length followed by fixed-size records. The format is inferred from the
/// length and `record_count` (see [`AttendanceFormat::detect`]).
///
/// # Errors
///
/// Fails only if the length prefix is missing or the format cannot be
/// determined. Individual bad records go to [`Batch::rejected`].
pub fn decode_table(data: &[u8], record_count: usize, enc: TextEncoding) -> Result<AttendanceBatch> {
    let (announced, _, _) = split_table(data)?;
    let format = AttendanceFormat::detect(announced, record_count)?;
    decode_table_as(data, format, enc)
}

/// Decode an attendance table with a known format.
///
/// # Errors
///
/// Fails only if the length prefix is missing.
pub fn decode_table_as(data: &[u8], format: AttendanceFormat, enc: TextEncoding) -> Result<AttendanceBatch> {
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
/// [`ProtoError::InvalidField`] if `rec` has the wrong length, or
/// [`ProtoError::InvalidTimestamp`] if the time field is not a real date.
pub fn decode_record(rec: &[u8], format: AttendanceFormat, enc: TextEncoding) -> Result<Attendance> {
    super::check_len(rec, format.record_len())?;
    match format {
        // uid u16 | status u8 | time u32 | punch u8
        AttendanceFormat::Short8 => Ok(Attendance {
            uid: Some(u16_at(rec, 0)),
            user_id: None,
            status: AttendanceStatus::from_byte(rec[2]),
            timestamp: time::decode_packed(u32_at(rec, 3))?,
            punch: Punch::from_byte(rec[7]),
            workcode: None,
        }),
        // user_id u32 | time u32 | status u8 | punch u8 | reserved[2] | workcode u32
        AttendanceFormat::Medium16 => Ok(Attendance {
            uid: None,
            user_id: Some(u32_at(rec, 0).to_string()),
            timestamp: time::decode_packed(u32_at(rec, 4))?,
            status: AttendanceStatus::from_byte(rec[8]),
            punch: Punch::from_byte(rec[9]),
            workcode: Some(u32_at(rec, 12)),
        }),
        // uid u16 | user_id[24] | status u8 | time u32 | punch u8 | reserved[8]
        AttendanceFormat::Long40 => Ok(Attendance {
            uid: Some(u16_at(rec, 0)),
            user_id: Some(enc.decode_field(&rec[2..26])),
            status: AttendanceStatus::from_byte(rec[26]),
            timestamp: time::decode_packed(u32_at(rec, 27))?,
            punch: Punch::from_byte(rec[31]),
            workcode: None,
        }),
        AttendanceFormat::Long49 if rec[..9] == PREFIX_49 => {
            decode_record(&rec[9..], AttendanceFormat::Long40, enc)
        }
        AttendanceFormat::Long49 => decode_record(&rec[..40], AttendanceFormat::Long40, enc),
    }
}

/// Encode one record.
///
/// # Errors
///
/// Fails if a field the layout needs is missing or out of range, or the
/// timestamp cannot be represented.
pub fn encode_record(a: &Attendance, format: AttendanceFormat, enc: TextEncoding) -> Result<Vec<u8>> {
    let missing = |field| ProtoError::InvalidField {
        field,
        reason: "required by this layout".into(),
    };
    let t = time::encode_packed(a.timestamp)?.to_le_bytes();
    let mut out = Vec::with_capacity(format.record_len());
    match format {
        AttendanceFormat::Short8 => {
            out.extend_from_slice(&a.uid.ok_or_else(|| missing("uid"))?.to_le_bytes());
            out.push(a.status.to_byte());
            out.extend_from_slice(&t);
            out.push(a.punch.to_byte());
        }
        AttendanceFormat::Medium16 => {
            let id = a.user_id.as_deref().ok_or_else(|| missing("user_id"))?;
            let id: u32 = id.parse().map_err(|_| ProtoError::InvalidField {
                field: "user_id",
                reason: format!("{id:?} is not a u32"),
            })?;
            out.extend_from_slice(&id.to_le_bytes());
            out.extend_from_slice(&t);
            out.push(a.status.to_byte());
            out.push(a.punch.to_byte());
            out.extend_from_slice(&[0, 0]);
            out.extend_from_slice(&a.workcode.unwrap_or(0).to_le_bytes());
        }
        AttendanceFormat::Long40 | AttendanceFormat::Long49 => {
            if format == AttendanceFormat::Long49 {
                out.extend_from_slice(&PREFIX_49);
            }
            out.extend_from_slice(&a.uid.ok_or_else(|| missing("uid"))?.to_le_bytes());
            let id = a.user_id.as_deref().ok_or_else(|| missing("user_id"))?;
            enc.encode_field("user_id", id, 24, &mut out)?;
            out.push(a.status.to_byte());
            out.extend_from_slice(&t);
            out.push(a.punch.to_byte());
            out.extend_from_slice(&[0; 8]);
        }
    }
    debug_assert_eq!(out.len(), format.record_len(), "encoded record length");
    Ok(out)
}

/// Encode a whole table: `u32` byte length followed by the records.
///
/// # Errors
///
/// As [`encode_record`].
pub fn encode_table(records: &[Attendance], format: AttendanceFormat, enc: TextEncoding) -> Result<Vec<u8>> {
    let mut body = Vec::with_capacity(records.len() * format.record_len());
    for r in records {
        body.extend(encode_record(r, format, enc)?);
    }
    let mut out = u32::try_from(body.len())
        .map_err(|_| ProtoError::Unsupported("attendance table larger than 4 GiB"))?
        .to_le_bytes()
        .to_vec();
    out.extend(body);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn rec(format: AttendanceFormat) -> Attendance {
        let has_uid = format != AttendanceFormat::Medium16;
        let has_user_id = format != AttendanceFormat::Short8;
        Attendance {
            uid: has_uid.then_some(42),
            user_id: has_user_id.then(|| "1001".to_string()),
            timestamp: NaiveDate::from_ymd_opt(2024, 2, 29)
                .unwrap()
                .and_hms_opt(8, 30, 5)
                .unwrap(),
            status: AttendanceStatus::Face,
            punch: Punch::CheckOut,
            workcode: (format == AttendanceFormat::Medium16).then_some(7),
        }
    }

    #[test]
    fn round_trip_all_formats() {
        let enc = TextEncoding::UTF8;
        for f in [
            AttendanceFormat::Short8,
            AttendanceFormat::Medium16,
            AttendanceFormat::Long40,
            AttendanceFormat::Long49,
        ] {
            let bytes = encode_record(&rec(f), f, enc).unwrap();
            assert_eq!(bytes.len(), f.record_len());
            assert_eq!(decode_record(&bytes, f, enc).unwrap(), rec(f), "{f:?}");
        }
    }

    #[test]
    fn bad_timestamp_is_isolated() {
        let enc = TextEncoding::UTF8;
        let f = AttendanceFormat::Short8;
        let mut table = encode_table(&[rec(f), rec(f), rec(f)], f, enc).unwrap();
        // Day slot 30 of February.
        let bad = ((23u32 * 12 + 1) * 31 + 30) * 86_400;
        table[4 + 8 + 3..4 + 8 + 7].copy_from_slice(&bad.to_le_bytes());
        let batch = decode_table(&table, 3, enc).unwrap();
        assert_eq!(batch.records.len(), 2);
        assert_eq!(batch.rejected.len(), 1);
        assert_eq!(batch.rejected[0].offset, 8);
        assert_eq!(batch.rejected[0].raw.len(), 8);
        assert_eq!(batch.rejected[0].reason, ProtoError::InvalidTimestamp(bad));
    }

    #[test]
    fn both_49_byte_layouts() {
        let enc = TextEncoding::UTF8;
        let prefixed = encode_record(&rec(AttendanceFormat::Long49), AttendanceFormat::Long49, enc).unwrap();
        assert_eq!(&prefixed[..9], &PREFIX_49);
        let mut suffixed = prefixed[9..].to_vec();
        suffixed.extend_from_slice(&[0xAB; 9]);
        for bytes in [prefixed, suffixed] {
            assert_eq!(
                decode_record(&bytes, AttendanceFormat::Long49, enc).unwrap(),
                rec(AttendanceFormat::Long49)
            );
        }
    }

    #[test]
    fn detect_is_strict() {
        assert_eq!(AttendanceFormat::detect(98, 2).unwrap(), AttendanceFormat::Long49);
        assert!(AttendanceFormat::detect(99, 2).is_err());
        assert!(AttendanceFormat::detect(48, 2).is_err());
        assert!(AttendanceFormat::detect(16, 0).is_err());
    }
}
