//! Byte-for-byte agreement with the reference implementation (pyzk).
//!
//! The fixtures in `tests/fixtures/generated/` are produced by
//! `tools/gen_fixtures.py`, which drives pyzk against a fake device and
//! records what it sends and how it parses what it receives.

use std::time::Duration;

use bytes::Bytes;
use chrono::NaiveDateTime;
use serde_json::Value;
use zkteco_proto::records::attendance::{self, AttendanceFormat};
use zkteco_proto::records::finger::{self, Template};
use zkteco_proto::records::sizes::DeviceSizes;
use zkteco_proto::records::user::{self, Privilege, User, UserFormat};
use zkteco_proto::records::{event, options};
use zkteco_proto::{commkey, framing, packet, requests, time, transfer};
use zkteco_proto::{ChecksumPolicy, Command, EventFlags, TextEncoding, Transport, Uid};

const ENC: TextEncoding = TextEncoding::UTF8;

fn cases(name: &str) -> Vec<Value> {
    let path = format!(
        "{}/tests/fixtures/generated/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let doc: Value = serde_json::from_str(&text).unwrap();
    match &doc["cases"] {
        Value::Array(a) => a.clone(),
        other => vec![other.clone()],
    }
}

fn hexv(v: &Value) -> Vec<u8> {
    hex::decode(v.as_str().unwrap()).unwrap()
}

fn num<T: TryFrom<i64>>(v: &Value) -> T {
    T::try_from(v.as_i64().unwrap()).ok().unwrap()
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap()
}

fn dt(v: &Value) -> NaiveDateTime {
    NaiveDateTime::parse_from_str(s(v), "%Y-%m-%dT%H:%M:%S").unwrap()
}

#[test]
fn packets() {
    let cs = cases("packets");
    assert!(cs.len() > 60);
    let mut stream = Vec::new();
    for c in &cs {
        let payload = hexv(&c["payload"]);
        let pkt = packet::encode(
            Command::from_u16(num(&c["command"])),
            num(&c["session_id"]),
            num(&c["reply_id"]),
            &payload,
        );
        assert_eq!(hex::encode(&pkt), s(&c["packet"]), "{c}");
        let frame = framing::encode_tcp(&pkt);
        assert_eq!(hex::encode(&frame), s(&c["tcp_frame"]));
        let decoded = packet::decode(Bytes::from(hexv(&c["packet"])), ChecksumPolicy::Reject).unwrap();
        assert_eq!(&decoded.payload[..], &payload[..]);
        stream.extend_from_slice(&frame);
    }
    // All frames back-to-back, fed in awkward 7-byte reads.
    let mut dec = framing::TcpFrameDecoder::default();
    let mut out = Vec::new();
    for piece in stream.chunks(7) {
        dec.extend(piece);
        while let Some(p) = dec.next_packet().unwrap() {
            out.push(hex::encode(p));
        }
    }
    let expected: Vec<_> = cs.iter().map(|c| s(&c["packet"]).to_owned()).collect();
    assert_eq!(out, expected);
}

#[test]
fn commkey_matches() {
    let cs = cases("commkey");
    assert!(cs.len() > 60);
    for c in cs {
        let key = commkey::derive(num(&c["password"]), num(&c["session_id"]), num(&c["ticks"]));
        assert_eq!(hex::encode(key), s(&c["key"]), "{c}");
    }
}

#[test]
fn time_encodings() {
    let c = &cases("time")[0];
    for p in c["packed"].as_array().unwrap() {
        let t = dt(&p["datetime"]);
        let v: u32 = num(&p["packed"]);
        assert_eq!(time::encode_packed(t).unwrap(), v);
        assert_eq!(time::decode_packed(v).unwrap(), t);
    }
    for p in c["compact"].as_array().unwrap() {
        let b: [u8; 6] = hexv(&p["bytes"]).try_into().unwrap();
        assert_eq!(time::decode_compact(b).unwrap(), dt(&p["datetime"]));
        assert_eq!(time::encode_compact(dt(&p["datetime"])).unwrap(), b);
    }
}

#[test]
fn user_tables() {
    let mut formats_seen = std::collections::HashSet::new();
    for c in cases("user_tables") {
        let table = hexv(&c["table"]);
        let batch = user::decode_table(&table, num(&c["user_count"]), ENC).unwrap();
        assert!(batch.is_clean(), "{:?}", batch.rejected);
        let expected = c["users"].as_array().unwrap();
        assert_eq!(batch.records.len(), expected.len());
        for (u, e) in batch.records.iter().zip(expected) {
            assert_eq!(u.uid, Uid(num(&e["uid"])));
            assert_eq!(u.privilege_byte(), num::<u8>(&e["privilege"]));
            assert_eq!(u.password, s(&e["password"]));
            assert_eq!(u.display_name(), s(&e["name"]));
            assert_eq!(u.card, num::<u32>(&e["card"]));
            assert_eq!(u.group_id, s(&e["group_id"]));
            assert_eq!(u.user_id, s(&e["user_id"]));
        }
        formats_seen.insert(num::<i64>(&c["record_len"]));
    }
    assert_eq!(formats_seen.len(), 3);
}

fn user_from(e: &Value) -> User {
    let p: u8 = num(&e["privilege"]);
    let mut user = User::new(Uid(num(&e["uid"])), s(&e["user_id"]).into());
    user.name = s(&e["name"]).into();
    user.password = s(&e["password"]).into();
    user.privilege = Privilege::from_byte(p);
    user.disabled = p & 1 == 1;
    user.card = num(&e["card"]);
    user.group_id = s(&e["group_id"]).into();
    user
}

fn user_format(c: &Value) -> UserFormat {
    UserFormat::from_record_len(num(&c["record_len"])).unwrap()
}

#[test]
fn user_writes() {
    for c in cases("user_writes") {
        let u = user_from(&c["user"]);
        let rec = user::encode_record(&u, user_format(&c), ENC).unwrap();
        assert_eq!(hex::encode(rec), s(&c["payload"]), "{c}");
    }
}

#[test]
fn attendance_tables() {
    for c in cases("attendance") {
        let table = hexv(&c["table"]);
        let format = AttendanceFormat::from_record_len(num(&c["record_len"])).unwrap();
        let batch = attendance::decode_table(&table, num(&c["record_count"]), ENC).unwrap();
        assert!(batch.is_clean(), "{:?}", batch.rejected);
        let expected = c["records"].as_array().unwrap();
        assert_eq!(batch.records.len(), expected.len());
        for (i, (a, e)) in batch.records.iter().zip(expected).enumerate() {
            assert_eq!(a.timestamp, dt(&e["timestamp"]));
            assert_eq!(a.status.to_byte(), num::<u8>(&e["status"]));
            assert_eq!(a.punch.to_byte(), num::<u8>(&e["punch"]));
            match format {
                AttendanceFormat::Short8 => assert_eq!(a.uid, Some(Uid(num(&e["uid"])))),
                AttendanceFormat::Medium16 => {
                    assert_eq!(a.user_id.as_deref(), Some(s(&e["user_id"])));
                    assert_eq!(a.workcode, Some(num(&c["workcodes"][i])));
                }
                AttendanceFormat::Long40 | AttendanceFormat::Long49 => {
                    assert_eq!(a.uid, Some(Uid(num(&e["uid"]))));
                    assert_eq!(a.user_id.as_deref(), Some(s(&e["user_id"])));
                }
            }
            // Re-encoding reproduces the original bytes.
            let rec = attendance::encode_record(a, format, ENC).unwrap();
            let len = format.record_len();
            assert_eq!(&rec[..], &table[4 + i * len..4 + (i + 1) * len]);
        }
    }
}

#[test]
fn live_events() {
    let cs = cases("events");
    assert!(cs.iter().any(|c| c["records"].as_array().unwrap().len() > 1));
    for c in cs {
        let batch = event::decode_attendance_payload(&hexv(&c["payload"]), ENC);
        assert!(batch.is_clean(), "{:?}", batch.rejected);
        let expected = c["records"].as_array().unwrap();
        assert_eq!(batch.records.len(), expected.len(), "{c}");
        for (a, e) in batch.records.iter().zip(expected) {
            assert_eq!(a.user_id.as_deref(), Some(s(&e["user_id"])));
            assert_eq!(a.timestamp, dt(&e["timestamp"]));
            assert_eq!(a.status.to_byte(), num::<u8>(&e["status"]));
            assert_eq!(a.punch.to_byte(), num::<u8>(&e["punch"]));
        }
    }
}

#[test]
fn template_tables() {
    for c in cases("templates") {
        let batch = finger::decode_table(&hexv(&c["table"])).unwrap();
        assert!(batch.is_clean(), "{:?}", batch.rejected);
        let expected = c["templates"].as_array().unwrap();
        assert_eq!(batch.records.len(), expected.len());
        for (t, e) in batch.records.iter().zip(expected) {
            assert_eq!(t.uid, Uid(num(&e["uid"])));
            assert_eq!(t.fid, num::<u8>(&e["fid"]));
            assert_eq!(t.valid, num::<u8>(&e["valid"]));
            assert_eq!(hex::encode(&t.data), s(&e["data"]));
        }
        assert_eq!(
            hex::encode(finger::encode_table(&batch.records).unwrap()),
            s(&c["table"])
        );
    }
}

#[test]
fn single_templates() {
    for c in cases("single_templates") {
        let req = requests::template_by_uid(Uid(num(&c["uid"])), num(&c["fid"]));
        assert_eq!(hex::encode(req), s(&c["request"]));
        let cleaned = finger::clean_single(&Bytes::from(hexv(&c["reply"]))).unwrap();
        assert_eq!(hex::encode(cleaned), s(&c["data"]));
    }
}

#[test]
fn bulk_save() {
    for c in cases("bulk_save") {
        let entries: Vec<(User, Vec<Template>)> = c["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                let u = user_from(&e["user"]);
                let ts = e["templates"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|t| Template::new(u.uid, num(&t["fid"]), Bytes::from(hexv(&t["data"]))))
                    .collect();
                (u, ts)
            })
            .collect();
        let buffer = finger::encode_bulk_save(&entries, user_format(&c), ENC).unwrap();
        assert_eq!(hex::encode(&buffer), s(&c["buffer"]));
        let len = u32::try_from(buffer.len()).unwrap();
        assert_eq!(hex::encode(requests::prepare_data(len)), s(&c["prepare_data"]));
        assert_eq!(
            hex::encode(finger::SAVE_USER_TEMPS_PAYLOAD),
            s(&c["save_payload"])
        );
        let lens: Vec<i64> = transfer::upload_packets(&buffer)
            .map(|p| i64::try_from(p.len()).unwrap())
            .collect();
        let expected: Vec<i64> = c["data_packet_lens"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_i64().unwrap())
            .collect();
        assert_eq!(lens, expected);
    }
}

#[test]
fn sizes() {
    for c in cases("sizes") {
        let d = DeviceSizes::decode(&hexv(&c["payload"])).unwrap();
        assert_eq!(d.users(), num::<u32>(&c["users"]));
        assert_eq!(d.fingers(), num::<u32>(&c["fingers"]));
        assert_eq!(d.records(), num::<u32>(&c["records"]));
        assert_eq!(d.cards(), num::<u32>(&c["cards"]));
        assert_eq!(d.fingers_capacity(), num::<u32>(&c["fingers_cap"]));
        assert_eq!(d.users_capacity(), num::<u32>(&c["users_cap"]));
        assert_eq!(d.records_capacity(), num::<u32>(&c["rec_cap"]));
        assert_eq!(d.fingers_available(), num::<u32>(&c["fingers_av"]));
        assert_eq!(d.users_available(), num::<u32>(&c["users_av"]));
        assert_eq!(d.records_available(), num::<u32>(&c["rec_av"]));
        assert_eq!(d.faces().unwrap_or(0), num::<u32>(&c["faces"]));
        assert_eq!(d.faces_capacity().unwrap_or(0), num::<u32>(&c["faces_cap"]));
        assert_eq!(hex::encode(d.encode()), s(&c["payload"]));
    }
}

#[test]
fn device_info() {
    let c = &cases("device_info")[0];
    let reply = |k: &str| hexv(&c["replies"][k]);
    let r = &c["results"];
    let text = |k: &str| options::decode_value(&reply(k));
    let int = |k: &str| options::parse_int(&text(k)).unwrap();
    assert_eq!(text("~SerialNumber"), s(&r["serial_number"]));
    assert_eq!(text("~Platform"), s(&r["platform"]));
    assert_eq!(text("MAC"), s(&r["mac"]));
    assert_eq!(text("~DeviceName"), s(&r["device_name"]));
    assert_eq!(int("ZKFaceVersion"), r["face_version"].as_i64().unwrap());
    assert_eq!(int("~ZKFPVersion"), r["fp_version"].as_i64().unwrap());
    assert_eq!(int("~ExtendFmt"), r["extend_fmt"].as_i64().unwrap());
    assert_eq!(int("~UserExtFmt"), r["user_extend_fmt"].as_i64().unwrap());
    assert_eq!(int("FaceFunOn"), r["face_fun_on"].as_i64().unwrap());
    assert_eq!(
        int("CompatOldFirmware"),
        r["compat_old_firmware"].as_i64().unwrap()
    );
    assert_eq!(text("IPAddress"), s(&r["network"]["ip"]));
    assert_eq!(text("NetMask"), s(&r["network"]["mask"]));
    assert_eq!(text("GATEIPAddress"), s(&r["network"]["gateway"]));
    assert_eq!(
        requests::decode_cstring(&reply("__version__")),
        s(&r["firmware_version"])
    );
    assert_eq!(
        requests::decode_pin_width(&reply("__pin_width__")).unwrap(),
        num::<u8>(&r["pin_width"])
    );
    assert_eq!(requests::decode_time(&reply("__time__")).unwrap(), dt(&r["time"]));

    let option_reads: Vec<String> = c["requests"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|q| num::<u16>(&q["command"]) == Command::OptionsRrq.to_u16())
        .map(|q| s(&q["payload"]).to_owned())
        .collect();
    let keys = [
        options::keys::SERIAL_NUMBER,
        options::keys::PLATFORM,
        options::keys::MAC,
        options::keys::DEVICE_NAME,
        options::keys::FACE_VERSION,
        options::keys::FP_VERSION,
        options::keys::EXTEND_FMT,
        options::keys::USER_EXT_FMT,
        options::keys::FACE_FUN_ON,
        options::keys::COMPAT_OLD_FIRMWARE,
        options::keys::IP_ADDRESS,
        options::keys::NET_MASK,
        options::keys::GATEWAY,
    ];
    let ours: Vec<String> = keys
        .iter()
        .map(|k| hex::encode(options::encode_read(k).unwrap()))
        .collect();
    assert_eq!(option_reads, ours);
}

#[test]
fn command_payloads() {
    let cs = cases("command_payloads");
    let mut checked = 0;
    for c in &cs {
        let name = s(&c["name"]);
        let parts: Vec<&str> = name.split(' ').collect();
        let arg = |i: usize| parts[i].parse::<u32>().unwrap();
        let ours: Vec<u8> = match parts[0] {
            "set_time" => {
                requests::set_time(NaiveDateTime::parse_from_str(parts[1], "%Y-%m-%dT%H:%M:%S").unwrap())
                    .unwrap()
                    .to_vec()
            }
            "unlock" => requests::unlock(Duration::from_secs(arg(1).into()))
                .unwrap()
                .to_vec(),
            "test_voice" => requests::test_voice(arg(1)).to_vec(),
            "write_lcd" => requests::write_lcd(1, parts[2], ENC).unwrap(),
            "reg_event" => requests::reg_event(EventFlags::from_bits_retain(arg(1))).to_vec(),
            "delete_user" => requests::delete_user(Uid(u16::try_from(arg(1)).unwrap())).to_vec(),
            "delete_user_template" => {
                requests::template_by_uid(Uid(u16::try_from(arg(1)).unwrap()), u8::try_from(arg(2)).unwrap())
                    .to_vec()
            }
            "get_pin_width" => requests::PIN_WIDTH_REQUEST.to_vec(),
            "sdk_build" => options::encode_write(options::keys::SDK_BUILD, "1").unwrap(),
            "prepare_buffer" => {
                let (cmd, table) = match parts[1] {
                    "users" => (Command::UserTempRrq, zkteco_proto::command::table::USERS),
                    "attendance" => (Command::AttLogRrq, 0),
                    _ => (Command::DbRrq, zkteco_proto::command::table::FINGER_TEMPLATES),
                };
                requests::prepare_buffer(cmd, table, 0).to_vec()
            }
            "enroll" if parts[1] == "tcp" => requests::start_enroll_tcp(&parts[2].into(), 3, ENC).unwrap(),
            "enroll" => requests::start_enroll_udp(&arg(2).to_string().into(), 3)
                .unwrap()
                .to_vec(),
            other => panic!("no encoder mapped for {other}"),
        };
        assert_eq!(hex::encode(ours), s(&c["payload"]), "{name}");
        checked += 1;
    }
    assert_eq!(checked, cs.len());
}

#[test]
fn chunk_plans() {
    for c in cases("chunked_reads") {
        let transport = if s(&c["transport"]) == "tcp" {
            Transport::Tcp
        } else {
            Transport::Udp
        };
        let plan: Vec<[i64; 2]> = transfer::chunk_plan(num(&c["size"]), transport)
            .map(|(a, b)| [a.into(), b.into()])
            .collect();
        let expected: Vec<[i64; 2]> = c["reads"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| [r[0].as_i64().unwrap(), r[1].as_i64().unwrap()])
            .collect();
        assert_eq!(plan, expected);
        assert!(c["free_data_sent"].as_bool().unwrap());
    }
}
