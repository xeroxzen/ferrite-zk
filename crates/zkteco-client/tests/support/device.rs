//! Mock device state and command handling.

use bytes::Bytes;
use chrono::{NaiveDate, NaiveDateTime};
use zkteco_proto::command::table;
use zkteco_proto::packet::{self, Packet};
use zkteco_proto::records::attendance::{self, Attendance, AttendanceFormat};
use zkteco_proto::records::finger::{self, Template};
use zkteco_proto::records::options;
use zkteco_proto::records::sizes::{index, DeviceSizes};
use zkteco_proto::records::user::{self, User, UserFormat};
use zkteco_proto::transfer::PrepareData;
use zkteco_proto::{commkey, requests, time, Command, EventFlags, TextEncoding, Uid, UserId};

/// How reply frames are written to the socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// One write per frame.
    Separate,
    /// All frames of one exchange in a single write.
    Coalesced,
    /// All frames concatenated, then written in pieces of this many bytes.
    Split(usize),
}

/// Faults to inject. "Once" faults clear themselves after firing.
#[derive(Debug, Clone)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent test switches, set one at a time"
)]
pub struct Faults {
    pub delivery: Delivery,
    /// Send no reply to the next request with this command.
    pub drop_reply_once: Option<Command>,
    /// Send no reply to any request with this command.
    pub drop_replies: Option<Command>,
    /// Reply `AckError` to every request with this command.
    pub ack_error: Option<Command>,
    /// Reply `AckUnauth` to every request with this command (after login).
    pub ack_unauth: Option<Command>,
    /// Corrupt the checksum of the next reply to this command.
    pub bad_checksum_once: Option<Command>,
    /// Close the connection instead of replying to this command.
    pub close_on: Option<Command>,
    /// Send a live event before replying to this command.
    pub event_before: Option<Command>,
    /// Stream the next chunk one byte short.
    pub short_stream_once: bool,
    /// Give the second `Data` packet of the next stream a wrong sequence.
    pub bad_sequence_once: bool,
    /// Send an `AckError` with a wrong reply ID before the real reply to the
    /// next request with this command.
    pub decoy_reply_once: Option<Command>,
    /// Give every reply to this command a wrong reply ID.
    pub wrong_reply_id: Option<Command>,
    /// Insert a live event after every `Data` packet of every stream.
    pub events_in_streams: bool,
    /// Leave out the `Data` packet with this index from the next stream (a
    /// lost datagram).
    pub lose_data_packet_once: Option<usize>,
    /// Cut the next stream short: leave out its last `Data` packet and the
    /// closing `AckOk` (the tail of a stream lost in transit).
    pub lose_stream_tail_once: bool,
    /// Send the `Data` packet with this index twice in the next stream (a
    /// duplicated datagram).
    pub duplicate_data_packet_once: Option<usize>,
    /// Swap the packets at this index and the next, counting from the first
    /// after `PrepareData`, in the next stream (datagrams reordered in
    /// transit). `Some(0)` with a single `Data` packet swaps it with the
    /// closing `AckOk`.
    pub reorder_stream_once: Option<usize>,
}

impl Default for Faults {
    fn default() -> Self {
        Self {
            delivery: Delivery::Separate,
            drop_reply_once: None,
            drop_replies: None,
            ack_error: None,
            ack_unauth: None,
            bad_checksum_once: None,
            close_on: None,
            event_before: None,
            short_stream_once: false,
            bad_sequence_once: false,
            decoy_reply_once: None,
            wrong_reply_id: None,
            events_in_streams: false,
            lose_data_packet_once: None,
            lose_stream_tail_once: false,
            duplicate_data_packet_once: None,
            reorder_stream_once: None,
        }
    }
}

/// Everything the mock device holds.
#[derive(Debug, Clone)]
pub struct DeviceState {
    pub password: Option<u32>,
    pub session_id: u16,
    pub encoding: TextEncoding,
    pub users: Vec<User>,
    pub user_format: UserFormat,
    pub attendance: Vec<Attendance>,
    pub attendance_format: AttendanceFormat,
    pub templates: Vec<Template>,
    pub firmware: String,
    pub options: Vec<(String, String)>,
    pub time: NaiveDateTime,
    /// Tables and templates up to this size are sent inline.
    pub inline_limit: usize,
    /// Payload size of streamed `Data` packets.
    pub data_packet_len: usize,
    pub faults: Faults,
    /// Every request received, in order.
    pub log: Vec<Packet>,
    pub connections: usize,
    /// Live events sent so far (their user IDs count up from 1).
    pub events_sent: u32,
    /// The last completed upload.
    pub uploaded: Option<Vec<u8>>,
    /// Upload buffers committed with `SaveUserTemps`.
    pub saved_bulk: Vec<Vec<u8>>,
    /// Attendance records (by index) whose stored time is made invalid
    /// (February 31st), so they are rejected when decoded. 40-byte layout
    /// only.
    pub corrupt_attendance: Vec<usize>,
    /// Event masks received with `RegEvent`, in order.
    pub event_masks: Vec<u32>,
    /// Report payloads sent as event packets right after acknowledging
    /// `StartEnroll`, with their flags.
    pub enroll_script: Vec<(EventFlags, Vec<u8>)>,
    /// `StartEnroll` payloads received.
    pub enroll_requests: Vec<Vec<u8>>,
    /// Unframed packets waiting to be pushed to the client unprompted.
    pub outbox: Vec<Vec<u8>>,
    /// Close the connection the next time the outbox is delivered.
    pub close_pushed: bool,
    /// Packets to send after the replies to the current request.
    pub(crate) trailing: Vec<Vec<u8>>,
    pub(crate) authenticated: bool,
    pub(crate) staged: Option<Vec<u8>>,
    pub(crate) upload: Option<(usize, Vec<u8>)>,
}

impl Default for DeviceState {
    fn default() -> Self {
        Self {
            password: None,
            session_id: 0x2A51,
            encoding: TextEncoding::UTF8,
            users: Vec::new(),
            user_format: UserFormat::Standard72,
            attendance: Vec::new(),
            attendance_format: AttendanceFormat::Long40,
            templates: Vec::new(),
            firmware: "Ver 6.60 Mock".into(),
            options: vec![
                ("~SerialNumber".into(), "MOCK0000000001".into()),
                ("~Platform".into(), "MOCK_TFT".into()),
                ("~DeviceName".into(), "Mock".into()),
                ("MAC".into(), "00:00:00:00:00:00".into()),
                ("IPAddress".into(), "127.0.0.1".into()),
                ("NetMask".into(), "255.0.0.0".into()),
                ("GATEIPAddress".into(), "0.0.0.0".into()),
            ],
            time: NaiveDate::from_ymd_opt(2026, 1, 2)
                .unwrap()
                .and_hms_opt(3, 4, 5)
                .unwrap(),
            inline_limit: 1024,
            data_packet_len: 1024,
            faults: Faults::default(),
            log: Vec::new(),
            connections: 0,
            events_sent: 0,
            uploaded: None,
            saved_bulk: Vec::new(),
            corrupt_attendance: Vec::new(),
            event_masks: Vec::new(),
            enroll_script: Vec::new(),
            enroll_requests: Vec::new(),
            outbox: Vec::new(),
            close_pushed: false,
            trailing: Vec::new(),
            authenticated: false,
            staged: None,
            upload: None,
        }
    }
}

/// What to send back for one request.
#[derive(Debug, Default)]
pub struct Outcome {
    /// Unframed reply packets, in order.
    pub replies: Vec<Vec<u8>>,
    /// Close the connection after sending them.
    pub close: bool,
}

impl DeviceState {
    pub fn handle(&mut self, request: &Packet) -> Outcome {
        self.log.push(request.clone());
        let cmd = request.header.command;
        if cmd == Command::AckOk {
            return Outcome::default(); // the host acknowledging an event
        }
        if self.faults.drop_reply_once == Some(cmd) {
            self.faults.drop_reply_once = None;
            return Outcome::default();
        }
        if self.faults.drop_replies == Some(cmd) {
            return Outcome::default();
        }
        if self.faults.close_on == Some(cmd) {
            return Outcome {
                replies: Vec::new(),
                close: true,
            };
        }
        let mut replies = Vec::new();
        if self.faults.event_before == Some(cmd) {
            replies.push(self.event_packet());
        }
        let mut reply_id = request.header.reply_id;
        if self.faults.decoy_reply_once == Some(cmd) {
            self.faults.decoy_reply_once = None;
            let decoy = packet::encode(
                Command::AckError,
                self.session_id,
                reply_id.wrapping_add(1000),
                &[],
            );
            replies.push(decoy.to_vec());
        }
        if self.faults.wrong_reply_id == Some(cmd) {
            reply_id = reply_id.wrapping_add(1000);
        }
        let (body, close) = self.dispatch(request);
        for (code, session, payload) in body {
            if code == Command::RegEvent {
                replies.push(self.event_packet());
            } else {
                replies.push(packet::encode(code, session, reply_id, &payload).to_vec());
            }
        }
        if self.faults.bad_checksum_once == Some(cmd) {
            self.faults.bad_checksum_once = None;
            if let Some(last) = replies.last_mut() {
                last[2] ^= 0xFF;
            }
        }
        replies.append(&mut self.trailing);
        Outcome { replies, close }
    }

    /// The attendance record of the latest event: user ID `events_sent`.
    pub fn last_event_record(&self) -> Attendance {
        Attendance {
            uid: Some(Uid(u16::try_from(self.events_sent).unwrap())),
            user_id: Some(UserId(self.events_sent.to_string())),
            timestamp: self.time,
            status: attendance::AttendanceStatus::Fingerprint,
            punch: attendance::Punch::CheckIn,
            workcode: None,
        }
    }

    /// An `ATTLOG` event repeating the punch of user ID `n`.
    pub fn event_packet_for(&self, n: u32) -> Vec<u8> {
        let mut a = self.last_event_record();
        a.user_id = Some(UserId(n.to_string()));
        let payload = zkteco_proto::records::event::encode_record(
            &a,
            zkteco_proto::records::event::EventRecordFormat::Numeric12,
            self.encoding,
        )
        .unwrap();
        event_with(EventFlags::ATTLOG, &payload)
    }

    /// An `ATTLOG` event for the next user ID (counting up from 1).
    pub fn event_packet(&mut self) -> Vec<u8> {
        self.events_sent += 1;
        let a = self.last_event_record();
        let payload = zkteco_proto::records::event::encode_record(
            &a,
            zkteco_proto::records::event::EventRecordFormat::Numeric12,
            self.encoding,
        )
        .unwrap();
        let flags = u16::try_from(EventFlags::ATTLOG.bits()).unwrap();
        packet::encode(Command::RegEvent, flags, 0, &payload).to_vec()
    }

    fn ok(&self, payload: Vec<u8>) -> Vec<(Command, u16, Vec<u8>)> {
        vec![(Command::AckOk, self.session_id, payload)]
    }

    fn reply(&self, code: Command) -> Vec<(Command, u16, Vec<u8>)> {
        vec![(code, self.session_id, Vec::new())]
    }

    /// Returns reply packets as (code, session field, payload) and whether
    /// to close afterwards.
    fn dispatch(&mut self, request: &Packet) -> (Vec<(Command, u16, Vec<u8>)>, bool) {
        let cmd = request.header.command;
        let p = &request.payload;
        match cmd {
            Command::Connect => return (self.connect(), false),
            Command::Auth => return (self.auth(p), false),
            _ if !self.authenticated => return (self.reply(Command::AckUnauth), false),
            _ if self.faults.ack_unauth == Some(cmd) => return (self.reply(Command::AckUnauth), false),
            _ if self.faults.ack_error == Some(cmd) => return (self.reply(Command::AckError), false),
            Command::Exit | Command::Restart | Command::PowerOff => return (self.ok(Vec::new()), true),
            _ => {}
        }
        let replies = match cmd {
            Command::PrepareBuffer => self.prepare_buffer(p),
            Command::ReadBuffer => self.read_buffer(p),
            Command::FreeData => {
                self.staged = None;
                self.ok(Vec::new())
            }
            Command::PrepareData | Command::Data => self.receive_upload(cmd, p),
            Command::GetUserTemp => self.user_template(p),
            _ => self.simple(cmd, p),
        };
        (replies, false)
    }

    fn connect(&mut self) -> Vec<(Command, u16, Vec<u8>)> {
        self.authenticated = self.password.is_none();
        let code = if self.authenticated {
            Command::AckOk
        } else {
            Command::AckUnauth
        };
        self.reply(code)
    }

    fn auth(&mut self, key: &[u8]) -> Vec<(Command, u16, Vec<u8>)> {
        let expected = commkey::derive(
            self.password.unwrap_or(0),
            self.session_id,
            commkey::DEFAULT_TICKS,
        );
        self.authenticated = key == expected;
        let code = if self.authenticated {
            Command::AckOk
        } else {
            Command::AckUnauth
        };
        self.reply(code)
    }

    fn table(&self, cmd: Command, fct: u32) -> Option<Vec<u8>> {
        let enc = self.encoding;
        match (cmd, fct) {
            (Command::UserTempRrq, table::USERS) => {
                user::encode_table(&self.users, self.user_format, enc).ok()
            }
            (Command::AttLogRrq, _) => {
                let mut table =
                    attendance::encode_table(&self.attendance, self.attendance_format, enc).ok()?;
                // 2000-02-31 in the packed form: day index 30 of month index 1.
                let invalid = (31u32 + 30) * 86_400;
                for &i in &self.corrupt_attendance {
                    let at = 4 + i * 40 + 27;
                    table[at..at + 4].copy_from_slice(&invalid.to_le_bytes());
                }
                Some(table)
            }
            (Command::DbRrq, table::FINGER_TEMPLATES) => finger::encode_table(&self.templates).ok(),
            _ => None,
        }
    }

    fn prepare_buffer(&mut self, p: &[u8]) -> Vec<(Command, u16, Vec<u8>)> {
        let Ok((cmd, fct, _)) = requests::decode_prepare_buffer(p) else {
            return self.reply(Command::AckError);
        };
        let Some(data) = self.table(cmd, fct) else {
            return self.reply(Command::AckError);
        };
        if data.len() <= self.inline_limit {
            return vec![(Command::Data, self.session_id, data)];
        }
        let size = u32::try_from(data.len()).unwrap();
        self.staged = Some(data);
        self.ok(requests::encode_prepare_buffer_ack(size).to_vec())
    }

    fn read_buffer(&mut self, p: &[u8]) -> Vec<(Command, u16, Vec<u8>)> {
        let Ok((start, size)) = requests::decode_read_buffer(p) else {
            return self.reply(Command::AckError);
        };
        let (start, size) = (start as usize, size as usize);
        let Some(chunk) = self
            .staged
            .as_ref()
            .and_then(|s| s.get(start..start + size))
            .map(<[u8]>::to_vec)
        else {
            return self.reply(Command::AckError);
        };
        self.stream(&chunk)
    }

    /// `PrepareData`, sequenced `Data` packets, `AckOk`, with stream faults.
    fn stream(&mut self, data: &[u8]) -> Vec<(Command, u16, Vec<u8>)> {
        let announce = PrepareData {
            size: u32::try_from(data.len()).unwrap(),
            packet_size: Some(u32::try_from(self.data_packet_len).unwrap()),
        };
        let mut out = vec![(Command::PrepareData, self.session_id, announce.encode())];
        let mut data = data.to_vec();
        if std::mem::take(&mut self.faults.short_stream_once) {
            data.pop();
        }
        let bad_seq = std::mem::take(&mut self.faults.bad_sequence_once);
        let lose = self.faults.lose_data_packet_once.take();
        let duplicate = self.faults.duplicate_data_packet_once.take();
        for (i, piece) in data.chunks(self.data_packet_len).enumerate() {
            let mut seq = u16::try_from(i).unwrap();
            if bad_seq && i == 1 {
                seq += 7;
            }
            if lose == Some(i) {
                continue;
            }
            out.push((Command::Data, seq, piece.to_vec()));
            if duplicate == Some(i) {
                out.push((Command::Data, seq, piece.to_vec()));
            }
            if self.faults.events_in_streams {
                // Placeholder; `handle` substitutes a real event packet.
                out.push((Command::RegEvent, 0, Vec::new()));
            }
        }
        if std::mem::take(&mut self.faults.lose_stream_tail_once) {
            let last_data = out.iter().rposition(|(c, _, _)| *c == Command::Data);
            if let Some(i) = last_data {
                out.truncate(i);
            }
            return out;
        }
        out.push((Command::AckOk, self.session_id, Vec::new()));
        if let Some(i) = self.faults.reorder_stream_once.take() {
            if i + 2 < out.len() {
                out.swap(i + 1, i + 2);
            }
        }
        out
    }

    fn user_template(&mut self, p: &[u8]) -> Vec<(Command, u16, Vec<u8>)> {
        let uid = Uid(u16::from_le_bytes([p[0], p[1]]));
        let fid = p[2];
        let Some(t) = self.templates.iter().find(|t| t.uid == uid && t.fid == fid) else {
            return self.reply(Command::AckError);
        };
        // The device appends one byte (see finger::clean_single).
        let mut data = t.data.to_vec();
        data.push(0);
        if data.len() <= self.inline_limit {
            return vec![(Command::Data, self.session_id, data)];
        }
        self.stream(&data)
    }

    fn receive_upload(&mut self, cmd: Command, p: &[u8]) -> Vec<(Command, u16, Vec<u8>)> {
        if cmd == Command::PrepareData {
            let size = u32::from_le_bytes([p[0], p[1], p[2], p[3]]) as usize;
            self.upload = Some((size, Vec::new()));
            return self.ok(Vec::new());
        }
        let Some((size, buf)) = self.upload.as_mut() else {
            return self.reply(Command::AckError);
        };
        buf.extend_from_slice(p);
        if buf.len() == *size {
            self.uploaded = self.upload.take().map(|(_, b)| b);
        }
        self.ok(Vec::new())
    }

    fn sizes(&self) -> Vec<u8> {
        let mut s = DeviceSizes::default();
        s.words[index::USERS] = u32::try_from(self.users.len()).unwrap();
        s.words[index::FINGERS] = u32::try_from(self.templates.len()).unwrap();
        s.words[index::RECORDS] = u32::try_from(self.attendance.len()).unwrap();
        s.encode()
    }

    /// Single-packet commands.
    fn simple(&mut self, cmd: Command, p: &[u8]) -> Vec<(Command, u16, Vec<u8>)> {
        match cmd {
            Command::GetFreeSizes => self.ok(self.sizes()),
            Command::GetVersion => self.ok([self.firmware.as_bytes(), &[0]].concat()),
            Command::OptionsRrq => self.read_option(p),
            Command::OptionsWrq => {
                let text = String::from_utf8_lossy(p).into_owned();
                let (k, v) = text.split_once('=').unwrap_or((&text, ""));
                self.options.push((k.to_string(), v.to_string()));
                self.ok(Vec::new())
            }
            Command::GetPinWidth => self.ok(vec![9]),
            Command::GetTime => self.ok(time::encode_packed(self.time).unwrap().to_le_bytes().to_vec()),
            Command::SetTime => {
                self.time = requests::decode_time(p).unwrap();
                self.ok(Vec::new())
            }
            Command::UserWrq => self.write_user(p),
            Command::DeleteUser => {
                let uid = Uid(u16::from_le_bytes([p[0], p[1]]));
                self.users.retain(|u| u.uid != uid);
                self.ok(Vec::new())
            }
            Command::DeleteUserTemp => self.delete_template(Uid(u16::from_le_bytes([p[0], p[1]])), p[2]),
            Command::DelUserTemp => {
                let id = self.encoding.decode_field(&p[..24]);
                let uid = self
                    .users
                    .iter()
                    .find(|u| u.user_id == id.as_str())
                    .map(|u| u.uid);
                match uid {
                    Some(uid) => self.delete_template(uid, p[24]),
                    None => self.reply(Command::AckError),
                }
            }
            Command::SaveUserTemps => {
                let buf = self.uploaded.clone().unwrap_or_default();
                let applied = self.apply_bulk(&buf);
                self.saved_bulk.push(buf);
                if applied.is_some() {
                    self.ok(Vec::new())
                } else {
                    self.reply(Command::AckError)
                }
            }
            Command::ClearAttLog | Command::ClearData => self.clear(cmd),
            Command::RegEvent => {
                self.event_masks
                    .push(u32::from_le_bytes([p[0], p[1], p[2], p[3]]));
                self.ok(Vec::new())
            }
            Command::StartEnroll => {
                self.enroll_requests.push(p.to_vec());
                let script = std::mem::take(&mut self.enroll_script);
                self.trailing = script.iter().map(|(f, r)| event_with(*f, r)).collect();
                self.ok(Vec::new())
            }
            Command::RefreshData
            | Command::EnableDevice
            | Command::DisableDevice
            | Command::TestVoice
            | Command::Unlock
            | Command::DoorStateRrq
            | Command::WriteLcd
            | Command::ClearLcd
            | Command::CancelCapture
            | Command::StartVerify => self.ok(Vec::new()),
            _ => self.reply(Command::AckUnknown),
        }
    }

    /// Apply a bulk save buffer (see `finger::encode_bulk_save`).
    fn apply_bulk(&mut self, buf: &[u8]) -> Option<()> {
        let word = |o: usize| {
            buf.get(o..o + 4)
                .map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize)
        };
        let (users_len, index_len, temps_len) = (word(0)?, word(4)?, word(8)?);
        let users = buf.get(12..12 + users_len)?;
        let index = buf.get(12 + users_len..12 + users_len + index_len)?;
        let temps_start = 12 + users_len + index_len;
        let temps = buf.get(temps_start..temps_start + temps_len)?;
        for rec in users.chunks(1 + self.user_format.record_len()) {
            let u = user::decode_record(&rec[1..], self.user_format, self.encoding).ok()?;
            self.users.retain(|x| x.uid != u.uid);
            self.users.push(u);
        }
        for e in index.chunks(8) {
            let uid = Uid(u16::from_le_bytes([e[1], e[2]]));
            let fid = e[3] - 0x10;
            let off = u32::from_le_bytes([e[4], e[5], e[6], e[7]]) as usize;
            let len = usize::from(u16::from_le_bytes([temps[off], temps[off + 1]]));
            let data = Bytes::copy_from_slice(temps.get(off + 2..off + 2 + len)?);
            self.templates.retain(|t| !(t.uid == uid && t.fid == fid));
            self.templates.push(Template {
                uid,
                fid,
                valid: 1,
                data,
            });
        }
        Some(())
    }

    fn clear(&mut self, cmd: Command) -> Vec<(Command, u16, Vec<u8>)> {
        self.attendance.clear();
        if cmd == Command::ClearData {
            self.users.clear();
            self.templates.clear();
        }
        self.ok(Vec::new())
    }

    fn read_option(&self, p: &[u8]) -> Vec<(Command, u16, Vec<u8>)> {
        let key = requests::decode_cstring(p);
        match self.options.iter().rev().find(|(k, _)| *k == key) {
            Some((k, v)) => self.ok(options::encode_reply(k, v)),
            None => self.reply(Command::AckError),
        }
    }

    fn write_user(&mut self, p: &[u8]) -> Vec<(Command, u16, Vec<u8>)> {
        match user::decode_record(p, self.user_format, self.encoding) {
            Ok(u) => {
                self.users.retain(|x| x.uid != u.uid);
                self.users.push(u);
                self.ok(Vec::new())
            }
            Err(_) => self.reply(Command::AckError),
        }
    }

    fn delete_template(&mut self, uid: Uid, fid: u8) -> Vec<(Command, u16, Vec<u8>)> {
        let before = self.templates.len();
        self.templates.retain(|t| !(t.uid == uid && t.fid == fid));
        if self.templates.len() < before {
            self.ok(Vec::new())
        } else {
            self.reply(Command::AckError)
        }
    }
}

/// Synthetic users with predictable fields.
pub fn users(n: u16) -> Vec<User> {
    (1..=n)
        .map(|uid| User {
            uid: Uid(uid),
            user_id: UserId(format!("{}", 1000 + u32::from(uid))),
            name: format!("User {uid}"),
            password: String::new(),
            privilege: user::Privilege::User,
            disabled: false,
            card: u32::from(uid) * 7,
            group_id: "1".into(),
        })
        .collect()
}

/// Synthetic attendance records.
pub fn punches(n: u32) -> Vec<Attendance> {
    let base = NaiveDate::from_ymd_opt(2026, 1, 1)
        .unwrap()
        .and_hms_opt(8, 0, 0)
        .unwrap();
    (0..n)
        .map(|i| Attendance {
            uid: Some(Uid(u16::try_from(i % 50 + 1).unwrap())),
            user_id: Some(UserId(format!("{}", 1001 + i % 50))),
            timestamp: base + chrono::Duration::minutes(i64::from(i)),
            status: attendance::AttendanceStatus::Fingerprint,
            punch: attendance::Punch::CheckIn,
            workcode: None,
        })
        .collect()
}

/// Synthetic templates (pseudo-random bytes, not biometric data).
pub fn templates(uids: &[u16], len: usize) -> Vec<Template> {
    uids.iter()
        .map(|&uid| Template {
            uid: Uid(uid),
            fid: 0,
            valid: 1,
            data: Bytes::from(
                (0..len)
                    .map(|i| u8::try_from((i * 31 + usize::from(uid)) % 251).unwrap() + 1)
                    .collect::<Vec<_>>(),
            ),
        })
        .collect()
}

/// An event packet with these flags and payload.
pub fn event_with(flags: EventFlags, payload: &[u8]) -> Vec<u8> {
    let flags = u16::try_from(flags.bits()).unwrap();
    packet::encode(Command::RegEvent, flags, 0, payload).to_vec()
}
