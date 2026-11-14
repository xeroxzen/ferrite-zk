//! Real-time event packets ([`Command::RegEvent`](crate::Command::RegEvent) sent by the device).

use bytes::Bytes;

use super::attendance::{Attendance, AttendanceStatus, Punch};
use super::ids::UserId;
use super::{u16_at, u32_at, Batch};
use crate::command::EventFlags;
use crate::error::{ProtoError, RecordKind, Result};
use crate::packet::Packet;
use crate::text::TextEncoding;
use crate::time;

/// Layouts of one attendance record inside an `EF_ATTLOG` event payload.
///
/// All start with a user ID, then status, punch and a 6-byte compact
/// timestamp; some end with bytes of unknown meaning, which are discarded.
#[doc = "Unverified against hardware."]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventRecordFormat {
    /// 10 bytes: u16 user ID.
    Numeric10,
    /// 12 bytes: u32 user ID.
    Numeric12,
    /// 14 bytes: u16 user ID, 4 unknown trailing bytes.
    Numeric14,
    /// 32 bytes: 24-byte user ID string.
    String32,
    /// 36 bytes: string user ID, 4 unknown trailing bytes.
    String36,
    /// 37 bytes: string user ID, 5 unknown trailing bytes.
    String37,
    /// 52 bytes: string user ID, 20 unknown trailing bytes. Several may be
    /// concatenated in one payload.
    String52,
}

impl EventRecordFormat {
    /// Size of one record.
    #[must_use]
    pub const fn record_len(self) -> usize {
        match self {
            Self::Numeric10 => 10,
            Self::Numeric12 => 12,
            Self::Numeric14 => 14,
            Self::String32 => 32,
            Self::String36 => 36,
            Self::String37 => 37,
            Self::String52 => 52,
        }
    }

    /// Choose the layout for the next record given the bytes remaining in
    /// the payload: an exact match on the shorter sizes, otherwise 52 bytes
    /// when at least that many remain.
    #[must_use]
    pub const fn for_remaining(len: usize) -> Option<Self> {
        match len {
            10 => Some(Self::Numeric10),
            12 => Some(Self::Numeric12),
            14 => Some(Self::Numeric14),
            32 => Some(Self::String32),
            36 => Some(Self::String36),
            37 => Some(Self::String37),
            n if n >= 52 => Some(Self::String52),
            _ => None,
        }
    }

    const fn id_len(self) -> usize {
        match self {
            Self::Numeric10 | Self::Numeric14 => 2,
            Self::Numeric12 => 4,
            _ => 24,
        }
    }
}

/// One item decoded from a live event packet.
///
/// Nothing in a packet is dropped: bytes that do not decode as attendance are
/// returned as [`LiveEvent::Raw`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveEvent {
    /// One attendance punch.
    Attendance(Attendance),
    /// Bytes that were not decoded: a non-attendance event, an attendance
    /// payload of unknown length, or a record that failed to decode.
    Raw {
        /// The event flags from the header (may be empty).
        flag: EventFlags,
        /// The undecoded bytes.
        payload: Bytes,
    },
}

/// Decode a live event packet into events, in payload order.
///
/// The event type comes from the header's `session_id` field. The payload is
/// decoded as attendance if the `ATTLOG` bit is set, or if no flag bit is set
/// and the payload length is a whole sequence of known attendance-event
/// lengths ([`is_attendance_length`]); the second case logs a warning.
/// Everything else becomes [`LiveEvent::Raw`]. An attendance payload yields
/// one [`LiveEvent::Attendance`] per record; a record that fails to decode,
/// or a remainder of unknown length, yields a `Raw` with those bytes. An empty
/// payload yields one empty `Raw`, so every packet produces at least one
/// event.
///
/// # Errors
///
/// [`ProtoError::UnexpectedReply`] if the packet is not a `RegEvent` packet.
pub fn decode(packet: &Packet, enc: TextEncoding) -> Result<Vec<LiveEvent>> {
    let Some(flag) = packet.header.event_flags() else {
        return Err(ProtoError::UnexpectedReply {
            got: packet.header.command,
            context: "decoding an event",
        });
    };
    let payload = &packet.payload;
    let as_attendance = if flag.contains(EventFlags::ATTLOG) {
        true
    } else if flag.is_empty() && is_attendance_length(payload.len()) {
        tracing::warn!(
            len = payload.len(),
            "event without flags decoded as attendance by payload length"
        );
        true
    } else {
        false
    };
    if !as_attendance || payload.is_empty() {
        return Ok(vec![LiveEvent::Raw {
            flag,
            payload: payload.clone(),
        }]);
    }
    let mut events = Vec::new();
    for_each_record(payload, |offset, rec, format| {
        let raw = || LiveEvent::Raw {
            flag,
            payload: payload.slice(offset..offset + rec.len()),
        };
        let event = match format.map(|f| decode_record(rec, f, enc)) {
            Some(Ok(a)) => LiveEvent::Attendance(a),
            Some(Err(e)) => {
                tracing::warn!(offset, len = rec.len(), %e, "undecodable attendance event record");
                raw()
            }
            None => {
                tracing::warn!(
                    offset,
                    len = rec.len(),
                    "attendance event bytes of unknown length"
                );
                raw()
            }
        };
        events.push(event);
    });
    Ok(events)
}

/// Whether `len` bytes split exactly into attendance-event records, using
/// the same per-record choice as decoding
/// ([`EventRecordFormat::for_remaining`]). Zero is not.
#[must_use]
pub fn is_attendance_length(len: usize) -> bool {
    let mut rest = len;
    while let Some(format) = EventRecordFormat::for_remaining(rest) {
        rest -= format.record_len();
        if rest == 0 {
            return true;
        }
    }
    false
}

/// Walk `payload` record by record, calling `f(offset, bytes, format)`. A
/// remainder that matches no layout is passed once with `None` and ends the
/// walk. Each step consumes at least 10 bytes, so the walk terminates.
fn for_each_record(payload: &[u8], mut f: impl FnMut(usize, &[u8], Option<EventRecordFormat>)) {
    let mut offset = 0;
    while offset < payload.len() {
        let rest = &payload[offset..];
        let Some(format) = EventRecordFormat::for_remaining(rest.len()) else {
            f(offset, rest, None);
            return;
        };
        f(offset, &rest[..format.record_len()], Some(format));
        offset += format.record_len();
    }
}

/// Decode an attendance event payload into one or more records.
///
/// Payloads of an unrecognised length are rejected, never skipped silently,
/// and decoding always terminates.
#[must_use]
pub fn decode_attendance_payload(payload: &[u8], enc: TextEncoding) -> Batch<Attendance> {
    let mut batch = Batch::default();
    for_each_record(payload, |offset, rec, format| {
        let Some(format) = format else {
            let reason = if rec.len() < 10 {
                ProtoError::Truncated {
                    what: "event record",
                    needed: 10,
                    available: rec.len(),
                }
            } else {
                ProtoError::UnsupportedRecordFormat {
                    kind: RecordKind::Event,
                    detail: format!("{}-byte attendance event payload", rec.len()),
                }
            };
            batch.reject(offset, rec, reason);
            return;
        };
        match decode_record(rec, format, enc) {
            Ok(a) => batch.records.push(a),
            Err(e) => batch.reject(offset, rec, e),
        }
    });
    batch
}

/// Decode one event record of exactly `format.record_len()` bytes.
///
/// # Errors
///
/// [`ProtoError::InvalidField`] if `rec` has the wrong length, or
/// [`ProtoError::InvalidCompactTimestamp`] if the time is not a real date.
pub fn decode_record(rec: &[u8], format: EventRecordFormat, enc: TextEncoding) -> Result<Attendance> {
    super::check_len(rec, format.record_len())?;
    let id_len = format.id_len();
    let user_id = match id_len {
        2 => u16_at(rec, 0).to_string(),
        4 => u32_at(rec, 0).to_string(),
        _ => enc.decode_field(&rec[..24]),
    };
    let mut t = [0u8; 6];
    t.copy_from_slice(&rec[id_len + 2..id_len + 8]);
    Ok(Attendance {
        uid: None,
        user_id: Some(UserId(user_id)),
        status: AttendanceStatus::from_byte(rec[id_len]),
        punch: Punch::from_byte(rec[id_len + 1]),
        timestamp: time::decode_compact(t)?,
        workcode: None,
    })
}

/// Encode one event record, with zeros in any unknown trailing bytes.
///
/// # Errors
///
/// Fails if the user ID does not fit the layout or the timestamp cannot be
/// represented.
pub fn encode_record(a: &Attendance, format: EventRecordFormat, enc: TextEncoding) -> Result<Vec<u8>> {
    let id = a
        .user_id
        .as_ref()
        .map(UserId::as_str)
        .ok_or(ProtoError::InvalidField {
            field: "user_id",
            reason: "required in events".into(),
        })?;
    let numeric = |reason: &str| ProtoError::InvalidField {
        field: "user_id",
        reason: format!("{id:?} is not a {reason}"),
    };
    let mut out = Vec::with_capacity(format.record_len());
    match format.id_len() {
        2 => out.extend_from_slice(&id.parse::<u16>().map_err(|_| numeric("u16"))?.to_le_bytes()),
        4 => out.extend_from_slice(&id.parse::<u32>().map_err(|_| numeric("u32"))?.to_le_bytes()),
        _ => enc.encode_field("user_id", id, 24, &mut out)?,
    }
    out.push(a.status.to_byte());
    out.push(a.punch.to_byte());
    out.extend_from_slice(&time::encode_compact(a.timestamp)?);
    out.resize(format.record_len(), 0);
    Ok(out)
}

/// Progress reports the device sends while enrolling a fingerprint.
///
/// Carried in event packets after `CMD_STARTENROLL`; the first `u16` of the
/// payload is a result code.
#[doc = "Unverified against hardware."]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrollReport {
    /// 0: the template was stored. Some firmware follows the code with the
    /// template size and position.
    Stored {
        /// Template size, when present.
        size: Option<u16>,
        /// Storage position, when present.
        position: Option<u16>,
    },
    /// 4: enrolment failed.
    Failed,
    /// 5: the finger is already enrolled.
    Duplicate,
    /// 6: the device timed out waiting for a finger.
    TimedOut,
    /// 0x64: a scan succeeded; place the finger again.
    ScanAccepted,
    /// Any other code.
    Other(u16),
}

/// Decode an enrolment report payload.
///
/// # Errors
///
/// [`ProtoError::Truncated`] if the payload is shorter than 2 bytes.
pub fn decode_enroll_report(payload: &[u8]) -> Result<EnrollReport> {
    crate::error::need("enroll report", payload, 2)?;
    let opt = |o: usize| (payload.len() >= o + 2).then(|| u16_at(payload, o));
    Ok(match u16_at(payload, 0) {
        0 => EnrollReport::Stored {
            size: opt(2),
            position: opt(4),
        },
        4 => EnrollReport::Failed,
        5 => EnrollReport::Duplicate,
        6 => EnrollReport::TimedOut,
        0x64 => EnrollReport::ScanAccepted,
        other => EnrollReport::Other(other),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::Command;
    use crate::packet::{self, ChecksumPolicy};
    use chrono::NaiveDate;

    fn att(id: &str) -> Attendance {
        Attendance {
            uid: None,
            user_id: Some(id.into()),
            timestamp: NaiveDate::from_ymd_opt(2024, 3, 1)
                .unwrap()
                .and_hms_opt(7, 45, 10)
                .unwrap(),
            status: AttendanceStatus::Face,
            punch: Punch::CheckIn,
            workcode: None,
        }
    }

    const ALL: [EventRecordFormat; 7] = [
        EventRecordFormat::Numeric10,
        EventRecordFormat::Numeric12,
        EventRecordFormat::Numeric14,
        EventRecordFormat::String32,
        EventRecordFormat::String36,
        EventRecordFormat::String37,
        EventRecordFormat::String52,
    ];

    #[test]
    fn every_layout_round_trips() {
        for f in ALL {
            let bytes = encode_record(&att("2001"), f, TextEncoding::UTF8).unwrap();
            assert_eq!(bytes.len(), f.record_len());
            let batch = decode_attendance_payload(&bytes, TextEncoding::UTF8);
            assert!(batch.is_clean(), "{f:?}");
            assert_eq!(batch.records, vec![att("2001")]);
        }
    }

    #[test]
    fn unexpected_lengths_are_rejected_not_spun_on() {
        for len in [11, 13, 15, 16, 20, 31, 33, 38, 40, 51] {
            let batch = decode_attendance_payload(&vec![0u8; len], TextEncoding::UTF8);
            assert!(batch.records.is_empty(), "{len}");
            assert_eq!(batch.rejected.len(), 1, "{len}");
            assert!(matches!(
                batch.rejected[0].reason,
                ProtoError::UnsupportedRecordFormat { .. }
            ));
        }
        let batch = decode_attendance_payload(&[0u8; 5], TextEncoding::UTF8);
        assert!(matches!(batch.rejected[0].reason, ProtoError::Truncated { .. }));
    }

    #[test]
    fn concatenated_52_byte_records() {
        let enc = TextEncoding::UTF8;
        let mut p = encode_record(&att("1"), EventRecordFormat::String52, enc).unwrap();
        p.extend(encode_record(&att("2"), EventRecordFormat::String52, enc).unwrap());
        p.extend(encode_record(&att("3"), EventRecordFormat::Numeric12, enc).unwrap());
        let batch = decode_attendance_payload(&p, enc);
        assert!(batch.is_clean());
        let ids: Vec<_> = batch.records.iter().map(|a| a.user_id.clone().unwrap()).collect();
        assert_eq!(ids, ["1", "2", "3"]);
    }

    fn event(flags: u16, payload: &[u8]) -> Packet {
        let raw = packet::encode(Command::RegEvent, flags, 0, payload).freeze();
        packet::decode(raw, ChecksumPolicy::Reject).unwrap()
    }

    fn record(id: &str, f: EventRecordFormat) -> Vec<u8> {
        encode_record(&att(id), f, TextEncoding::UTF8).unwrap()
    }

    fn raw(flag: EventFlags, payload: &[u8]) -> LiveEvent {
        LiveEvent::Raw {
            flag,
            payload: Bytes::copy_from_slice(payload),
        }
    }

    #[test]
    fn attlog_flag_set_decodes_attendance() {
        let p = record("7", EventRecordFormat::Numeric12);
        let got = decode(&event(1, &p), TextEncoding::UTF8).unwrap();
        assert_eq!(got, vec![LiveEvent::Attendance(att("7"))]);
    }

    #[test]
    fn missing_flag_with_known_length_decodes_attendance() {
        let mut p = record("1", EventRecordFormat::String52);
        p.extend(record("2", EventRecordFormat::Numeric10));
        let got = decode(&event(0, &p), TextEncoding::UTF8).unwrap();
        assert_eq!(
            got,
            vec![LiveEvent::Attendance(att("1")), LiveEvent::Attendance(att("2"))]
        );
    }

    #[test]
    fn missing_flag_with_unknown_length_is_raw() {
        for len in [1, 9, 11, 16, 40, 51, 63] {
            let p = vec![0xA5; len];
            let got = decode(&event(0, &p), TextEncoding::UTF8).unwrap();
            assert_eq!(got, vec![raw(EventFlags::empty(), &p)], "{len}");
        }
    }

    #[test]
    fn attlog_with_unknown_length_keeps_bytes() {
        let p = vec![0u8; 16];
        let got = decode(&event(1, &p), TextEncoding::UTF8).unwrap();
        assert_eq!(got, vec![raw(EventFlags::ATTLOG, &p)]);

        let mut p = record("1", EventRecordFormat::String52);
        p.extend([9u8; 5]);
        let got = decode(&event(1, &p), TextEncoding::UTF8).unwrap();
        assert_eq!(
            got,
            vec![LiveEvent::Attendance(att("1")), raw(EventFlags::ATTLOG, &[9; 5])]
        );
    }

    #[test]
    fn undecodable_record_is_raw_and_siblings_survive() {
        let mut bad = record("1", EventRecordFormat::String52);
        bad[26..32].copy_from_slice(&[24, 2, 30, 0, 0, 0]);
        let mut p = bad.clone();
        p.extend(record("2", EventRecordFormat::String52));
        let got = decode(&event(1, &p), TextEncoding::UTF8).unwrap();
        assert_eq!(
            got,
            vec![raw(EventFlags::ATTLOG, &bad), LiveEvent::Attendance(att("2"))]
        );
    }

    #[test]
    fn other_events_and_empty_payloads_are_raw() {
        let got = decode(&event(512, &[55, 0, 0, 0]), TextEncoding::UTF8).unwrap();
        assert_eq!(got, vec![raw(EventFlags::ALARM, &[55, 0, 0, 0])]);
        // Another flag set: never guessed as attendance, whatever the length.
        let p = record("7", EventRecordFormat::Numeric12);
        let got = decode(&event(512, &p), TextEncoding::UTF8).unwrap();
        assert_eq!(got, vec![raw(EventFlags::ALARM, &p)]);
        for flags in [0, 1] {
            let got = decode(&event(flags, &[]), TextEncoding::UTF8).unwrap();
            assert_eq!(got, vec![raw(EventFlags::from_bits_retain(flags.into()), &[])]);
        }
    }

    #[test]
    fn non_event_packet_is_an_error() {
        let raw = packet::encode(Command::AckOk, 1, 0, &[]).freeze();
        let p = packet::decode(raw, ChecksumPolicy::Reject).unwrap();
        assert!(decode(&p, TextEncoding::UTF8).is_err());
    }

    #[test]
    fn attendance_lengths() {
        for len in [10, 12, 14, 32, 36, 37, 52, 62, 64, 104, 156, 89] {
            assert!(is_attendance_length(len), "{len}");
        }
        for len in [0, 9, 11, 16, 40, 51, 53, 63, 110] {
            assert!(!is_attendance_length(len), "{len}");
        }
    }

    #[test]
    fn enroll_reports() {
        assert_eq!(
            decode_enroll_report(&[0x64, 0]).unwrap(),
            EnrollReport::ScanAccepted
        );
        assert_eq!(
            decode_enroll_report(&[0, 0, 0, 2, 3, 0]).unwrap(),
            EnrollReport::Stored {
                size: Some(512),
                position: Some(3)
            }
        );
        assert_eq!(
            decode_enroll_report(&[0, 0]).unwrap(),
            EnrollReport::Stored {
                size: None,
                position: None
            }
        );
        assert!(decode_enroll_report(&[0]).is_err());
    }
}
