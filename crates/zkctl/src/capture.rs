//! Turn a [`Recording`] into a scrubbed capture in the
//! `tests/fixtures/captures` format.
//!
//! Scrubbing follows the rules in `tests/fixtures/captures/README.md`:
//!
//! - user and attendance table data is reassembled from the `Data` packets
//!   (by request offset), scrubbed with [`zkteco_proto::scrub::Scrubber`],
//!   and written back into the same packets; with no known layout, it is
//!   zeroed;
//! - option replies that identify the device or its network are scrubbed;
//! - the `CMD_AUTH` payload is zeroed: the comm key can be reversed into the
//!   device password;
//! - `Data` packets that belong to no known request are zeroed;
//! - live events are left out (counted in `notes`).
//!
//! Lengths never change; changed packets get a fresh checksum.

use std::collections::HashMap;

use bytes::Bytes;
use serde_json::{json, Value};
use zkteco_client::{Direction, Recording};
use zkteco_proto::command::table;
use zkteco_proto::framing::encode_tcp;
use zkteco_proto::packet::{self, Packet};
use zkteco_proto::records::attendance::AttendanceFormat;
use zkteco_proto::records::user::UserFormat;
use zkteco_proto::scrub::{self, Scrubber};
use zkteco_proto::{requests, ChecksumPolicy, Command, Transport};

/// Descriptive fields of a capture file.
#[derive(Debug, Clone)]
pub struct Meta {
    /// `~DeviceName`.
    pub device: String,
    /// Firmware version.
    pub firmware: String,
    /// `~Platform`.
    pub platform: String,
    /// The transport the packets went over.
    pub transport: Transport,
    /// Free text.
    pub notes: String,
}

/// Layouts used to scrub table data.
#[derive(Debug, Clone, Copy, Default)]
pub struct Layouts {
    /// User records, if known.
    pub users: Option<UserFormat>,
    /// Attendance records, if known.
    pub attendance: Option<AttendanceFormat>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Table {
    Users,
    Attendance,
    Other,
}

#[derive(Debug, Clone, Copy)]
enum Request {
    Option,
    Stage(Table),
    Read { table: Table, offset: usize },
    Other,
}

/// A device `Data` payload that belongs at `offset` in a table image.
struct Segment {
    packet: usize,
    table: Table,
    offset: usize,
}

/// One packet kept in the capture.
struct Entry {
    direction: Direction,
    packet: Packet,
    /// The bytes as recorded, used if nothing changed.
    raw: Bytes,
    changed: bool,
}

/// Packets decoded for scrubbing.
struct Work {
    packets: Vec<Entry>,
    segments: Vec<Segment>,
    events: usize,
}

/// Build the scrubbed capture.
#[must_use]
pub fn build(rec: &Recording, meta: &Meta, layouts: Layouts) -> Value {
    let mut work = classify(rec);
    scrub_tables(&mut work, layouts);
    let tcp = meta.transport == Transport::Tcp;
    let entries: Vec<Value> = work
        .packets
        .iter()
        .map(|e| {
            let p = &e.packet;
            let raw = if e.changed {
                packet::encode(
                    p.header.command,
                    p.header.session_id,
                    p.header.reply_id,
                    &p.payload,
                )
                .freeze()
            } else {
                e.raw.clone()
            };
            let bytes = if tcp {
                encode_tcp(&raw).to_vec()
            } else {
                raw.to_vec()
            };
            let direction = match e.direction {
                Direction::HostToDevice => "host_to_device",
                Direction::DeviceToHost => "device_to_host",
            };
            json!({ "direction": direction, "hex": hex::encode(bytes) })
        })
        .collect();
    let mut notes = vec![meta.notes.clone()];
    if work.events > 0 {
        notes.push(format!("{} live event packets left out", work.events));
    }
    if rec.truncated {
        notes.push("recording truncated".into());
    }
    let notes = notes.join("; ");
    json!({
        "device": meta.device,
        "firmware": meta.firmware,
        "platform": meta.platform,
        "transport": if tcp { "tcp" } else { "udp" },
        "encoding": "utf-8",
        "synthetic_templates": false,
        "notes": notes,
        "packets": entries,
    })
}

/// Decode every packet, scrub what can be scrubbed packet by packet, and
/// note which `Data` payloads belong to which table.
fn classify(rec: &Recording) -> Work {
    let mut work = Work {
        packets: Vec::with_capacity(rec.packets.len()),
        segments: Vec::new(),
        events: 0,
    };
    let mut requests: HashMap<u16, Request> = HashMap::new();
    let mut cursors: HashMap<u16, usize> = HashMap::new();
    let mut staged = Table::Other;
    for r in &rec.packets {
        let Ok(mut p) = packet::decode(r.bytes.clone(), ChecksumPolicy::Ignore) else {
            // Undecodable: keep nothing of it.
            continue;
        };
        let changed = match r.direction {
            Direction::HostToDevice => host_packet(&mut p, &mut requests, &mut staged),
            Direction::DeviceToHost if p.header.command == Command::RegEvent => {
                work.events += 1;
                continue;
            }
            Direction::DeviceToHost => {
                let index = work.packets.len();
                device_packet(&mut p, index, &requests, &mut cursors, &mut work.segments)
            }
        };
        work.packets.push(Entry {
            direction: r.direction,
            packet: p,
            raw: r.bytes.clone(),
            changed,
        });
    }
    work
}

fn host_packet(p: &mut Packet, requests: &mut HashMap<u16, Request>, staged: &mut Table) -> bool {
    let request = match p.header.command {
        Command::OptionsRrq => Request::Option,
        Command::PrepareBuffer => {
            *staged = staged_table(&p.payload);
            Request::Stage(*staged)
        }
        Command::ReadBuffer => match requests::decode_read_buffer(&p.payload) {
            Ok((offset, _)) => Request::Read {
                table: *staged,
                offset: usize::try_from(offset).unwrap_or(usize::MAX),
            },
            Err(_) => Request::Other,
        },
        Command::Auth | Command::Data => {
            p.payload = Bytes::from(vec![0; p.payload.len()]);
            requests.insert(p.header.reply_id, Request::Other);
            return true;
        }
        _ => Request::Other,
    };
    requests.insert(p.header.reply_id, request);
    false
}

/// The table a `PrepareBuffer` payload stages: `u8`, `u16` command, `u32`
/// table selector, `u32`.
fn staged_table(payload: &[u8]) -> Table {
    let (Some(c), Some(t)) = (payload.get(1..3), payload.get(3..7)) else {
        return Table::Other;
    };
    let command = Command::from(u16::from_le_bytes([c[0], c[1]]));
    let selector = u32::from_le_bytes([t[0], t[1], t[2], t[3]]);
    match command {
        Command::UserTempRrq if selector == table::USERS => Table::Users,
        Command::AttLogRrq => Table::Attendance,
        _ => Table::Other,
    }
}

fn device_packet(
    p: &mut Packet,
    index: usize,
    requests: &HashMap<u16, Request>,
    cursors: &mut HashMap<u16, usize>,
    segments: &mut Vec<Segment>,
) -> bool {
    let reply_id = p.header.reply_id;
    let request = requests.get(&reply_id).copied().unwrap_or(Request::Other);
    match (request, p.header.command) {
        (Request::Option, _) => {
            let mut payload = p.payload.to_vec();
            let changed = scrub::option_reply(&mut payload);
            p.payload = payload.into();
            changed
        }
        (Request::Read { .. }, Command::PrepareData) => {
            cursors.insert(reply_id, 0);
            false
        }
        (Request::Stage(table), Command::Data) if table != Table::Other => {
            segments.push(Segment {
                packet: index,
                table,
                offset: 0,
            });
            false
        }
        (Request::Read { table, offset }, Command::Data) if table != Table::Other => {
            let cursor = cursors.entry(reply_id).or_insert(0);
            segments.push(Segment {
                packet: index,
                table,
                offset: offset.saturating_add(*cursor),
            });
            *cursor = cursor.saturating_add(p.payload.len());
            false
        }
        (_, Command::Data) => {
            p.payload = Bytes::from(vec![0; p.payload.len()]);
            true
        }
        _ => false,
    }
}

/// Reassemble each table, scrub it, and write it back into its packets.
fn scrub_tables(work: &mut Work, layouts: Layouts) {
    let mut images: HashMap<Table, Vec<u8>> = HashMap::new();
    for s in &work.segments {
        let p = &work.packets[s.packet].packet;
        let image = images.entry(s.table).or_default();
        let end = s.offset.saturating_add(p.payload.len());
        if image.len() < end {
            image.resize(end, 0);
        }
        image[s.offset..end].copy_from_slice(&p.payload);
    }
    let mut scrubber = Scrubber::new();
    for (table, image) in &mut images {
        match (table, layouts.users, layouts.attendance) {
            (Table::Users, Some(f), _) => scrubber.user_table(image, f),
            (Table::Attendance, _, Some(f)) => scrubber.attendance_table(image, f),
            _ => image.fill(0),
        }
    }
    for s in &work.segments {
        let entry = &mut work.packets[s.packet];
        let end = s.offset + entry.packet.payload.len();
        entry.packet.payload = Bytes::copy_from_slice(&images[&s.table][s.offset..end]);
        entry.changed = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zkteco_client::RecordedPacket;

    fn rec(packets: &[(Direction, Command, u16, &[u8])]) -> Recording {
        Recording {
            packets: packets
                .iter()
                .map(|&(direction, c, r, p)| RecordedPacket {
                    direction,
                    bytes: packet::encode(c, 7, r, p).freeze(),
                })
                .collect(),
            truncated: false,
        }
    }

    fn meta() -> Meta {
        Meta {
            device: "K40".into(),
            firmware: "Ver 6.60".into(),
            platform: "ZMM220_TFT".into(),
            transport: Transport::Udp,
            notes: "test".into(),
        }
    }

    fn payloads(capture: &Value) -> Vec<Vec<u8>> {
        capture["packets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                let raw = hex::decode(e["hex"].as_str().unwrap()).unwrap();
                packet::decode(raw.into(), ChecksumPolicy::Reject)
                    .unwrap()
                    .payload
                    .to_vec()
            })
            .collect()
    }

    use Direction::{DeviceToHost as D, HostToDevice as H};

    #[test]
    fn auth_events_options_and_stray_data() {
        let r = rec(&[
            (H, Command::Auth, 1, &[1, 2, 3, 4]),
            (D, Command::AckOk, 1, b""),
            (H, Command::OptionsRrq, 2, b"~SerialNumber\0"),
            (D, Command::AckOk, 2, b"~SerialNumber=AB12\0"),
            (D, Command::RegEvent, 0, &[9; 40]),
            (D, Command::Data, 77, b"secret"),
        ]);
        let c = build(&r, &meta(), Layouts::default());
        let p = payloads(&c);
        assert_eq!(p.len(), 5, "the event is left out");
        assert_eq!(p[0], [0, 0, 0, 0]);
        assert_eq!(p[3], b"~SerialNumber=0000\0");
        assert_eq!(p[4], [0; 6]);
        assert!(c["notes"].as_str().unwrap().contains("1 live event"));
    }

    #[test]
    fn table_without_a_known_layout_is_zeroed() {
        let mut stage = vec![1];
        stage.extend_from_slice(&u16::from(Command::AttLogRrq).to_le_bytes());
        stage.extend_from_slice(&[0; 8]);
        let r = rec(&[
            (H, Command::PrepareBuffer, 1, &stage),
            (D, Command::Data, 1, &[8, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8]),
        ]);
        let p = payloads(&build(&r, &meta(), Layouts::default()));
        assert_eq!(p[1], [0; 12]);
    }
}
