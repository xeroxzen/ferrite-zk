//! Device timestamp encodings.
//!
//! Device time is local wall-clock time with no zone information, so every
//! timestamp in this crate is a [`NaiveDateTime`]. Interpreting it in a time
//! zone is up to the caller, who knows where the device is installed.

use chrono::{Datelike, NaiveDate, NaiveDateTime, Timelike};

use crate::error::{ProtoError, Result};

const SECS_PER_DAY: u32 = 86_400;

/// Encode a timestamp as the packed 32-bit value used by attendance records
/// and `CMD_GET_TIME`/`CMD_SET_TIME`.
///
/// The value is `((yy * 12 + month0) * 31 + day0) * 86400 + seconds_of_day`,
/// with `yy` the year minus 2000 and every month given 31 day slots.
///
/// # Errors
///
/// [`ProtoError::YearOutOfRange`] outside 2000..=2099.
pub fn encode_packed(t: NaiveDateTime) -> Result<u32> {
    let yy = u32::try_from(t.year() - 2000)
        .ok()
        .filter(|y| *y < 100)
        .ok_or(ProtoError::YearOutOfRange(t.year()))?;
    let days = (yy * 12 + t.month0()) * 31 + t.day0();
    Ok(days * SECS_PER_DAY + t.num_seconds_from_midnight())
}

/// Decode a packed 32-bit timestamp.
///
/// # Errors
///
/// [`ProtoError::InvalidTimestamp`] if the value names a date that does not
/// exist (for example 31 February), which the encoding allows.
pub fn decode_packed(v: u32) -> Result<NaiveDateTime> {
    let secs = v % SECS_PER_DAY;
    let days = v / SECS_PER_DAY;
    let day = days % 31 + 1;
    let month = (days / 31) % 12 + 1;
    let year = i32::try_from(days / 31 / 12).map_err(|_| ProtoError::InvalidTimestamp(v))? + 2000;
    NaiveDate::from_ymd_opt(year, month, day)
        .and_then(|d| d.and_hms_opt(secs / 3600, secs / 60 % 60, secs % 60))
        .ok_or(ProtoError::InvalidTimestamp(v))
}

/// Encode a timestamp as the 6-byte form used in real-time events:
/// year minus 2000, month, day, hour, minute, second.
///
/// # Errors
///
/// [`ProtoError::YearOutOfRange`] outside 2000..=2255.
pub fn encode_compact(t: NaiveDateTime) -> Result<[u8; 6]> {
    let yy = u8::try_from(t.year() - 2000).map_err(|_| ProtoError::YearOutOfRange(t.year()))?;
    // chrono bounds every field below 60, so the conversion cannot fail.
    let byte = |v: u32| u8::try_from(v).unwrap_or(u8::MAX);
    Ok([
        yy,
        byte(t.month()),
        byte(t.day()),
        byte(t.hour()),
        byte(t.minute()),
        byte(t.second()),
    ])
}

/// Decode the 6-byte real-time event timestamp.
///
/// # Errors
///
/// [`ProtoError::InvalidCompactTimestamp`] if the bytes do not name a real
/// date and time.
pub fn decode_compact(b: [u8; 6]) -> Result<NaiveDateTime> {
    NaiveDate::from_ymd_opt(2000 + i32::from(b[0]), u32::from(b[1]), u32::from(b[2]))
        .and_then(|d| d.and_hms_opt(u32::from(b[3]), u32::from(b[4]), u32::from(b[5])))
        .ok_or(ProtoError::InvalidCompactTimestamp(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, mo, d)
            .unwrap()
            .and_hms_opt(h, mi, s)
            .unwrap()
    }

    #[test]
    fn packed_known_values() {
        assert_eq!(encode_packed(dt(2000, 1, 1, 0, 0, 0)).unwrap(), 0);
        assert_eq!(encode_packed(dt(2000, 1, 1, 0, 0, 1)).unwrap(), 1);
        assert_eq!(encode_packed(dt(2000, 1, 2, 0, 0, 0)).unwrap(), 86_400);
        assert_eq!(encode_packed(dt(2000, 2, 1, 0, 0, 0)).unwrap(), 31 * 86_400);
        assert_eq!(encode_packed(dt(2001, 1, 1, 0, 0, 0)).unwrap(), 12 * 31 * 86_400);
        let t = dt(2099, 12, 31, 23, 59, 59);
        assert_eq!(decode_packed(encode_packed(t).unwrap()).unwrap(), t);
    }

    #[test]
    fn packed_rejects_out_of_range_years() {
        assert_eq!(
            encode_packed(dt(1999, 12, 31, 0, 0, 0)),
            Err(ProtoError::YearOutOfRange(1999))
        );
        assert_eq!(
            encode_packed(dt(2100, 1, 1, 0, 0, 0)),
            Err(ProtoError::YearOutOfRange(2100))
        );
    }

    #[test]
    fn packed_rejects_impossible_dates() {
        // 2023-02-31: month slot 1, day slot 30.
        let v = ((23 * 12 + 1) * 31 + 30) * 86_400;
        assert_eq!(decode_packed(v), Err(ProtoError::InvalidTimestamp(v)));
    }

    #[test]
    fn compact_round_trip_and_rejects() {
        let t = dt(2023, 6, 15, 8, 4, 2);
        assert_eq!(encode_compact(t).unwrap(), [23, 6, 15, 8, 4, 2]);
        assert_eq!(decode_compact([23, 6, 15, 8, 4, 2]).unwrap(), t);
        assert!(decode_compact([23, 13, 15, 8, 4, 2]).is_err());
        assert!(decode_compact([23, 2, 30, 0, 0, 0]).is_err());
        assert!(decode_compact([23, 2, 1, 24, 0, 0]).is_err());
    }
}
