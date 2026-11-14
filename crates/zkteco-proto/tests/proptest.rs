//! Property tests: encode/decode round trips, and decoders never panicking
//! or looping on arbitrary input.

use bytes::Bytes;
use chrono::{NaiveDate, NaiveDateTime};
use proptest::prelude::*;
use zkteco_proto::records::attendance::{self, Attendance, AttendanceFormat, AttendanceStatus, Punch};
use zkteco_proto::records::event::{self, EventRecordFormat, LiveEvent};
use zkteco_proto::records::finger::{self, Template};
use zkteco_proto::records::sizes::DeviceSizes;
use zkteco_proto::records::user::{self, Privilege, User, UserFormat};
use zkteco_proto::transfer::{ChunkReceiver, Progress};
use zkteco_proto::{framing, packet, time, ChecksumPolicy, Command, TextEncoding, Transport, Uid, UserId};

const ENC: TextEncoding = TextEncoding::UTF8;

fn datetime() -> impl Strategy<Value = NaiveDateTime> {
    (2000i32..=2099, 1u32..=12, 1u32..=31, 0u32..24, 0u32..60, 0u32..60)
        .prop_filter_map("real date", |(y, mo, d, h, mi, s)| {
            NaiveDate::from_ymd_opt(y, mo, d).and_then(|d| d.and_hms_opt(h, mi, s))
        })
}

/// Text of at most `max` UTF-8 bytes with no NUL.
fn text(max: usize) -> impl Strategy<Value = String> {
    proptest::collection::vec(
        prop_oneof![
            proptest::char::range('a', 'z'),
            proptest::char::range('0', '9'),
            Just('é'),
            Just('ł')
        ],
        0..=max,
    )
    .prop_map(move |cs| {
        let mut s = String::new();
        for c in cs {
            if s.len() + c.len_utf8() <= max {
                s.push(c);
            }
        }
        s
    })
}

fn privilege() -> impl Strategy<Value = Privilege> {
    prop_oneof![
        Just(Privilege::User),
        Just(Privilege::Enroller),
        Just(Privilege::Manager),
        Just(Privilege::Admin)
    ]
}

fn user_for(format: UserFormat) -> BoxedStrategy<User> {
    let (pw, name, compact) = match format {
        UserFormat::Compact28 => (5, 8, true),
        _ => (8, 24, false),
    };
    (
        any::<u16>(),
        privilege(),
        any::<bool>(),
        text(pw),
        text(name),
        any::<u32>(),
        any::<u8>(),
        any::<u32>(),
        text(7),
        text(24),
    )
        .prop_map(
            move |(uid, privilege, disabled, password, name, card, g, id, gs, ids)| User {
                uid: Uid(uid),
                user_id: UserId(if compact { id.to_string() } else { ids }),
                name,
                password,
                privilege,
                disabled,
                card,
                group_id: if compact { g.to_string() } else { gs },
            },
        )
        .boxed()
}

fn status() -> impl Strategy<Value = AttendanceStatus> {
    any::<u8>().prop_map(AttendanceStatus::from_byte)
}

fn punch() -> impl Strategy<Value = Punch> {
    any::<u8>().prop_map(Punch::from_byte)
}

fn attendance_for(format: AttendanceFormat) -> BoxedStrategy<Attendance> {
    (
        any::<u16>(),
        any::<u32>(),
        text(24),
        datetime(),
        status(),
        punch(),
        any::<u32>(),
    )
        .prop_map(move |(uid, num_id, str_id, timestamp, status, punch, wc)| {
            let (uid, user_id, workcode) = match format {
                AttendanceFormat::Short8 => (Some(Uid(uid)), None, None),
                AttendanceFormat::Medium16 => (None, Some(UserId(num_id.to_string())), Some(wc)),
                _ => (Some(Uid(uid)), Some(UserId(str_id)), None),
            };
            Attendance {
                uid,
                user_id,
                timestamp,
                status,
                punch,
                workcode,
            }
        })
        .boxed()
}

const USER_FORMATS: [UserFormat; 2] = [UserFormat::Compact28, UserFormat::Standard72];
const ATT_FORMATS: [AttendanceFormat; 4] = [
    AttendanceFormat::Short8,
    AttendanceFormat::Medium16,
    AttendanceFormat::Long40,
    AttendanceFormat::Long49,
];
const EVENT_FORMATS: [EventRecordFormat; 7] = [
    EventRecordFormat::Numeric10,
    EventRecordFormat::Numeric12,
    EventRecordFormat::Numeric14,
    EventRecordFormat::String32,
    EventRecordFormat::String36,
    EventRecordFormat::String37,
    EventRecordFormat::String52,
];

proptest! {
    #[test]
    fn packet_round_trip(cmd in any::<u16>(), session in any::<u16>(), reply in any::<u16>(),
                         payload in proptest::collection::vec(any::<u8>(), 0..64)) {
        let raw = packet::encode(Command::from_u16(cmd), session, reply, &payload).freeze();
        let p = packet::decode(raw, ChecksumPolicy::Reject).unwrap();
        prop_assert_eq!(p.header.command.to_u16(), cmd);
        prop_assert_eq!(p.header.session_id, session);
        prop_assert_eq!(p.header.reply_id, reply);
        prop_assert_eq!(&p.payload[..], &payload[..]);
    }

    #[test]
    fn corrupted_byte_fails_checksum(payload in proptest::collection::vec(any::<u8>(), 0..32),
                                     idx in any::<prop::sample::Index>(), flip in 1u8..=255) {
        // Changing one byte changes one 16-bit word by a non-zero amount below
        // 0xFFFF, which always changes a ones'-complement sum.
        let mut raw = packet::encode(Command::AckOk, 1, 2, &payload).to_vec();
        let i = idx.index(raw.len());
        raw[i] ^= flip;
        prop_assert!(packet::decode(Bytes::from(raw), ChecksumPolicy::Reject).is_err());
    }

    #[test]
    fn frames_survive_any_split(payloads in proptest::collection::vec(proptest::collection::vec(any::<u8>(), 0..40), 1..6),
                                cuts in proptest::collection::vec(any::<prop::sample::Index>(), 0..10)) {
        let packets: Vec<_> = payloads.iter().enumerate()
            .map(|(i, p)| packet::encode(Command::Data, u16::try_from(i).unwrap(), 0, p).to_vec())
            .collect();
        let stream: Vec<u8> = packets.iter().flat_map(|p| framing::encode_tcp(p).to_vec()).collect();
        let mut points: Vec<usize> = cuts.iter().map(|c| c.index(stream.len() + 1)).collect();
        points.push(0);
        points.push(stream.len());
        points.sort_unstable();
        let mut dec = framing::TcpFrameDecoder::default();
        let mut out = Vec::new();
        for w in points.windows(2) {
            dec.extend(&stream[w[0]..w[1]]);
            while let Some(p) = dec.next_packet().unwrap() {
                out.push(p.to_vec());
            }
        }
        prop_assert_eq!(out, packets);
        prop_assert_eq!(dec.buffered(), 0);
    }

    #[test]
    fn frame_decoder_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..200)) {
        let mut dec = framing::TcpFrameDecoder::new(256);
        dec.extend(&bytes);
        for _ in 0..bytes.len() {
            match dec.next_packet() {
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => break,
            }
        }
    }

    #[test]
    fn packed_time_round_trip(t in datetime()) {
        prop_assert_eq!(time::decode_packed(time::encode_packed(t).unwrap()).unwrap(), t);
        prop_assert_eq!(time::decode_compact(time::encode_compact(t).unwrap()).unwrap(), t);
    }

    #[test]
    fn packed_time_decode_is_total(v in any::<u32>()) {
        if let Ok(t) = time::decode_packed(v) {
            if let Ok(back) = time::encode_packed(t) {
                prop_assert_eq!(back, v);
            }
        }
    }

    #[test]
    fn user_round_trip((f, u) in prop::sample::select(USER_FORMATS.to_vec()).prop_flat_map(|f| (Just(f), user_for(f)))) {
        let rec = user::encode_record(&u, f, ENC).unwrap();
        prop_assert_eq!(rec.len(), f.record_len());
        prop_assert_eq!(user::decode_record(&rec, f, ENC).unwrap(), u);
    }

    #[test]
    fn compact_users_round_trip(users in proptest::collection::vec(user_for(UserFormat::Compact28), 1..8)) {
        let table = user::encode_table(&users, UserFormat::Compact28, ENC).unwrap();
        let b = user::decode_table(&table, users.len(), ENC).unwrap();
        prop_assert!(b.is_clean());
        prop_assert_eq!(b.records, users);
    }

    #[test]
    fn standard_users_round_trip(users in proptest::collection::vec(user_for(UserFormat::Standard72), 1..8)) {
        let table = user::encode_table(&users, UserFormat::Standard72, ENC).unwrap();
        let b = user::decode_table(&table, users.len(), ENC).unwrap();
        prop_assert!(b.is_clean());
        prop_assert_eq!(b.records, users);
    }

    #[test]
    fn attendance_round_trip((f, records) in prop::sample::select(ATT_FORMATS.to_vec())
        .prop_flat_map(|f| (Just(f), proptest::collection::vec(attendance_for(f), 1..6)))) {
        let table = attendance::encode_table(&records, f, ENC).unwrap();
        let b = attendance::decode_table(&table, records.len(), ENC).unwrap();
        prop_assert!(b.is_clean());
        prop_assert_eq!(b.records, records);
    }

    #[test]
    fn event_round_trip(i in 0usize..7, t in datetime(), st in status(), p in punch(),
                        n16 in 1u16.., n32 in 1u32.., s in text(24)) {
        let f = EVENT_FORMATS[i];
        let id = match f {
            EventRecordFormat::Numeric10 | EventRecordFormat::Numeric14 => n16.to_string(),
            EventRecordFormat::Numeric12 => n32.to_string(),
            _ => s,
        };
        let a = Attendance { uid: None, user_id: Some(UserId(id)), timestamp: t, status: st, punch: p, workcode: None };
        let rec = event::encode_record(&a, f, ENC).unwrap();
        let b = event::decode_attendance_payload(&rec, ENC);
        prop_assert!(b.is_clean());
        prop_assert_eq!(b.records, vec![a]);
    }

    #[test]
    fn template_table_round_trip(ts in proptest::collection::vec(
        (any::<u16>(), 0u8..=9, any::<u8>(), proptest::collection::vec(any::<u8>(), 0..300)), 0..6)) {
        let ts: Vec<Template> = ts.into_iter()
            .map(|(uid, fid, valid, data)| Template { uid: Uid(uid), fid, valid, data: Bytes::from(data) })
            .collect();
        let b = finger::decode_table(&finger::encode_table(&ts).unwrap()).unwrap();
        prop_assert!(b.is_clean());
        prop_assert_eq!(b.records, ts);
    }

    #[test]
    fn table_decoders_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..400), count in 0usize..10) {
        let _ = user::decode_table(&bytes, count, ENC);
        for f in [UserFormat::Compact28, UserFormat::Standard72, UserFormat::NgTeco120] {
            let _ = user::decode_table_as(&bytes, f, ENC);
        }
        let _ = attendance::decode_table(&bytes, count, ENC);
        for f in ATT_FORMATS {
            let _ = attendance::decode_table_as(&bytes, f, ENC);
        }
        let _ = finger::decode_table(&bytes);
        let _ = DeviceSizes::decode(&bytes);
        let b = event::decode_attendance_payload(&bytes, ENC);
        // Every byte is accounted for: in a record or in a rejection.
        let rejected: usize = b.rejected.iter().map(|r| r.raw.len()).sum();
        let accepted_upper = bytes.len() - rejected;
        prop_assert!(b.records.len() * 10 <= accepted_upper);
    }

    #[test]
    fn live_events_never_drop_a_packet(flags in prop_oneof![Just(0u16), Just(1), any::<u16>()],
                                       bytes in proptest::collection::vec(any::<u8>(), 0..300)) {
        let raw = packet::encode(Command::RegEvent, flags, 0, &bytes).freeze();
        let p = packet::decode(raw, ChecksumPolicy::Reject).unwrap();
        let events = event::decode(&p, ENC).unwrap();
        prop_assert!(!events.is_empty());
        let raw_len: usize = events
            .iter()
            .map(|e| match e {
                LiveEvent::Raw { payload, .. } => payload.len(),
                LiveEvent::Attendance(_) => 0,
            })
            .sum();
        let decoded = events.len() - events.iter().filter(|e| matches!(e, LiveEvent::Raw { .. })).count();
        // Decoded records take at least 10 bytes each; the rest is returned raw.
        prop_assert!(raw_len + decoded * 10 <= bytes.len().max(raw_len));
        if decoded == 0 {
            prop_assert_eq!(raw_len, bytes.len());
        }
    }

    #[test]
    fn chunk_receiver_terminates(cmds in proptest::collection::vec((0usize..4, any::<u16>(), proptest::collection::vec(any::<u8>(), 0..12)), 0..20),
                                 tcp in any::<bool>()) {
        let transport = if tcp { Transport::Tcp } else { Transport::Udp };
        let mut r = ChunkReceiver::new(None, transport);
        for (k, seq, payload) in cmds {
            let cmd = [Command::PrepareData, Command::Data, Command::AckOk, Command::AckError][k];
            let raw = packet::encode(cmd, seq, 0, &payload).freeze();
            let p = packet::decode(raw, ChecksumPolicy::Reject).unwrap();
            match r.on_packet(&p) {
                Ok(Progress::NeedMore) => {}
                Ok(Progress::Complete(_)) | Err(_) => break,
            }
        }
    }
}
