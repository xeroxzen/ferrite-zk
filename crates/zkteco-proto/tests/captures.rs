//! Replays packet captures from real devices, if any are present, and
//! enforces the biometric-data rule from the captures README.
//!
//! Captures live in `tests/fixtures/captures/` at the repository root (see
//! the README there). They are excluded from the published crate, so these
//! tests do nothing outside the repository.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use zkteco_proto::command::table;
use zkteco_proto::framing::TcpFrameDecoder;
use zkteco_proto::records::event::{self, LiveEvent};
use zkteco_proto::{packet, requests, ChecksumPolicy, Command, EventFlags, Packet, TextEncoding};

/// Payloads at least this long, in a template context, count as templates.
/// Real fingerprint templates are several hundred bytes; the margin is wide
/// on purpose so the check fails safe.
const TEMPLATE_SIZED: usize = 64;

fn capture_files() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/captures");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut files: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    files
}

fn load(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap())
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dir {
    ToDevice,
    ToHost,
}

/// Every packet in the capture, in the order its last byte was captured.
/// TCP entries are reassembled per direction, so chunks may split or join
/// frames.
fn ordered_packets(doc: &Value) -> Result<Vec<(Dir, Packet)>, String> {
    let tcp = match doc["transport"].as_str() {
        Some("tcp") => true,
        Some("udp") => false,
        other => return Err(format!("transport {other:?}")),
    };
    let entries = doc["packets"].as_array().ok_or("missing packets")?;
    let mut decoders = [TcpFrameDecoder::default(), TcpFrameDecoder::default()];
    let mut out = Vec::new();
    for (i, entry) in entries.iter().enumerate() {
        let dir = match entry["direction"].as_str() {
            Some("host_to_device") => Dir::ToDevice,
            Some("device_to_host") => Dir::ToHost,
            other => return Err(format!("entry {i}: direction {other:?}")),
        };
        let hex = entry["hex"].as_str().ok_or(format!("entry {i}: missing hex"))?;
        let bytes = hex::decode(hex).map_err(|e| format!("entry {i}: {e}"))?;
        let mut raws = Vec::new();
        if tcp {
            let decoder = &mut decoders[usize::from(dir == Dir::ToHost)];
            decoder.extend(&bytes);
            while let Some(p) = decoder.next_packet().map_err(|e| format!("entry {i}: {e}"))? {
                raws.push(p);
            }
        } else {
            raws.push(bytes.into());
        }
        for raw in raws {
            let p = packet::decode(raw, ChecksumPolicy::Reject).map_err(|e| format!("entry {i}: {e}"))?;
            out.push((dir, p));
        }
    }
    if decoders.iter().any(|d| d.buffered() > 0) {
        return Err("trailing bytes after the last frame".into());
    }
    Ok(out)
}

/// Whether a host request starts an exchange whose replies carry templates.
fn opens_template_context(p: &Packet) -> bool {
    match p.header.command {
        Command::GetUserTemp | Command::DbRrq => true,
        Command::PrepareBuffer => requests::decode_prepare_buffer(&p.payload)
            .map_or(true, |(_, fct, _)| fct == table::FINGER_TEMPLATES),
        _ => false,
    }
}

/// Indices (into [`ordered_packets`]) of packets whose payload is
/// template-sized and template-bearing: every host-to-device `Data` upload
/// (only bulk template saves upload), and device replies while a template
/// read is in progress. The context lasts until the host sends anything
/// other than `ReadBuffer`.
fn template_payloads(packets: &[(Dir, Packet)]) -> Vec<usize> {
    let mut in_context = false;
    let mut found = Vec::new();
    for (i, (dir, p)) in packets.iter().enumerate() {
        let sized = p.payload.len() >= TEMPLATE_SIZED;
        match dir {
            Dir::ToDevice => {
                if p.header.command == Command::Data && sized {
                    found.push(i);
                }
                if p.header.command != Command::ReadBuffer {
                    in_context = opens_template_context(p);
                }
            }
            Dir::ToHost => {
                if in_context && sized {
                    found.push(i);
                }
            }
        }
    }
    found
}

fn marked_synthetic(doc: &Value) -> bool {
    doc["synthetic_templates"] == Value::Bool(true)
}

#[test]
fn replay_captures() {
    for path in capture_files() {
        let doc = load(&path);
        let enc = doc["encoding"].as_str().map_or(TextEncoding::UTF8, |l| {
            TextEncoding::for_label(l).expect("known encoding")
        });
        let packets = ordered_packets(&doc).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        for (dir, p) in &packets {
            if *dir != Dir::ToHost || p.header.command != Command::RegEvent {
                continue;
            }
            let flags = p.header.event_flags().unwrap_or(EventFlags::empty());
            let events = event::decode(p, enc).unwrap();
            if flags.contains(EventFlags::ATTLOG) {
                let raw: Vec<_> = events
                    .iter()
                    .filter(|e| matches!(e, LiveEvent::Raw { .. }))
                    .collect();
                assert!(raw.is_empty(), "{}: {raw:?}", path.display());
            }
        }
    }
}

#[test]
fn captures_contain_no_unmarked_templates() {
    for path in capture_files() {
        let doc = load(&path);
        if marked_synthetic(&doc) {
            continue;
        }
        let packets = ordered_packets(&doc).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let found = template_payloads(&packets);
        assert!(
            found.is_empty(),
            "{}: packets {found:?} carry template-sized payloads. Replace the template \
             bytes with synthetic bytes of the same length, recompute checksums, and set \
             \"synthetic_templates\": true (see tests/fixtures/captures/README.md)",
            path.display()
        );
    }
}

/// Self-test of the template check on synthetic captures.
mod check {
    use super::*;

    fn tcp(cmd: Command, session: u16, payload: &[u8]) -> String {
        let p = packet::encode(cmd, session, 0, payload);
        hex::encode(zkteco_proto::framing::encode_tcp(&p))
    }

    fn capture(entries: &[(&str, String)]) -> Value {
        let packets: Vec<_> = entries
            .iter()
            .map(|(d, h)| json!({ "direction": d, "hex": h }))
            .collect();
        json!({ "transport": "tcp", "packets": packets })
    }

    const H: &str = "host_to_device";
    const D: &str = "device_to_host";

    fn found(doc: &Value) -> Vec<usize> {
        template_payloads(&ordered_packets(doc).unwrap())
    }

    #[test]
    fn single_template_reply_is_flagged() {
        let doc = capture(&[
            (H, tcp(Command::GetUserTemp, 1, &[1, 0, 0])),
            (D, tcp(Command::AckOk, 1, &[0xA5; 600])),
        ]);
        assert_eq!(found(&doc), [1]);
    }

    #[test]
    fn buffered_template_read_is_flagged_but_user_read_is_not() {
        let fingers = requests::prepare_buffer(Command::DbRrq, table::FINGER_TEMPLATES, 0);
        let users = requests::prepare_buffer(Command::UserTempRrq, table::USERS, 0);
        let doc = capture(&[
            (H, tcp(Command::PrepareBuffer, 1, &users)),
            (D, tcp(Command::Data, 1, &[0; 300])),
            (H, tcp(Command::PrepareBuffer, 1, &fingers)),
            (
                D,
                tcp(Command::AckOk, 1, &requests::encode_prepare_buffer_ack(2000)),
            ),
            (H, tcp(Command::ReadBuffer, 1, &requests::read_buffer(0, 2000))),
            (D, tcp(Command::Data, 1, &[0; 2000])),
            (H, tcp(Command::FreeData, 1, &[])),
            (D, tcp(Command::AckOk, 1, &[0; 100])),
        ]);
        assert_eq!(found(&doc), [5]);
    }

    #[test]
    fn uploads_are_flagged_and_marking_exempts() {
        let mut doc = capture(&[
            (H, tcp(Command::Data, 1, &[7; 1024])),
            (H, tcp(Command::Data, 1, &[7; 8])),
        ]);
        assert_eq!(found(&doc), [0]);
        assert!(!marked_synthetic(&doc));
        doc["synthetic_templates"] = Value::Bool(true);
        assert!(marked_synthetic(&doc));
    }

    #[test]
    fn split_and_coalesced_frames_keep_order() {
        let a = tcp(Command::GetUserTemp, 1, &[1, 0, 0]);
        let b = tcp(Command::AckOk, 1, &[1; 200]);
        let doc = capture(&[(H, a), (D, b[..20].to_string()), (D, b[20..].to_string())]);
        assert_eq!(found(&doc), [1]);
    }
}
