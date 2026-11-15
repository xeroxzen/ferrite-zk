//! `zkctl diagnose`: a read-only survey of one device, reported as one row
//! of `docs/COMPATIBILITY.md` plus the measurements behind it.
//!
//! Every connection is opened with
//! [`ClientConfig::read_only`](zkteco_client::ClientConfig::read_only), so
//! the client itself refuses any command outside
//! [`READ_ONLY_COMMANDS`](zkteco_client::READ_ONLY_COMMANDS). Templates are
//! never read. The optional live probe ([`crate::live_probe`]) also allows
//! `CMD_REG_EVENT`.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde::Serialize;
use serde_json::Value;
use zkteco_client::compat::DeviceFacts;
use zkteco_client::zkteco_proto::records::sizes::DeviceSizes;
use zkteco_client::{Client, ClientConfig, Error, RawTable, Recording, ReplyIdPolicy, Transport};
use zkteco_proto::records::attendance::{self, AttendanceFormat};
use zkteco_proto::records::options::keys;
use zkteco_proto::records::user::{self, UserFormat};
use zkteco_proto::TextEncoding;

use crate::capture::{self, Layouts, Meta};
use crate::live_probe::{self, LiveProbe};
use crate::streams::{self, StreamHeaders};

/// Placeholder for identifiers left out of the report.
pub const SCRUBBED: &str = "scrubbed";

/// What to connect to and what to include.
#[derive(Debug, Clone)]
pub struct Options {
    /// Device host name or address.
    pub host: String,
    /// Device port.
    pub port: u16,
    /// Comm key, if the device has one.
    pub password: Option<u32>,
    /// Transport for the main connection.
    pub transport: Transport,
    /// Per-reply timeout.
    pub timeout: Duration,
    /// Include serial number and MAC in the report.
    pub include_identifiers: bool,
    /// Build scrubbed captures of each read.
    pub capture: bool,
    /// Listen for live events this long after the table probes.
    pub probe_live: Option<Duration>,
}

/// The report: one compatibility row and the evidence for it.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    /// The `docs/COMPATIBILITY.md` row this device would add.
    pub row: Row,
    /// Device details not in the row.
    pub device: Device,
    /// One read of both tables per transport.
    pub probes: Vec<Probe>,
    /// What streamed UDP `Data` packets carry in their header, from one
    /// 16 KiB sample (headers only; no data is kept).
    pub udp_stream_headers: Option<StreamHeaders>,
    /// Why there are no stream headers, if there are none.
    pub udp_stream_note: Option<String>,
    /// Whether each UDP probe's table bytes equal the TCP probe's.
    pub udp_matches_tcp: Vec<Comparison>,
    /// The live-event probe, if asked for.
    pub live: Option<LiveProbe>,
    /// Tool version.
    pub tool: String,
}

/// One row of `docs/COMPATIBILITY.md`, column for column.
#[derive(Debug, Clone, Serialize)]
pub struct Row {
    /// `~DeviceName`.
    pub model: String,
    /// Firmware version.
    pub firmware: String,
    /// `~Platform`.
    pub platform: String,
    /// `~UserExtFmt`.
    pub user_ext_fmt: String,
    /// `~ExtendFmt`.
    pub extend_fmt: String,
    /// Bytes per user record, or `-`.
    pub user_record: String,
    /// Bytes per attendance record, or `-`.
    pub attendance_record: String,
    /// Reply-ID behaviour on the main transport.
    pub reply_ids: String,
    /// Transports whose probe read both tables.
    pub transport: String,
    /// Left empty for the maintainer who checks the report.
    pub verified_by: String,
}

/// Device details outside the row.
#[derive(Debug, Clone, Serialize)]
pub struct Device {
    /// The transport the main connection used.
    pub connected_over: String,
    /// `~SerialNumber`, unless scrubbed.
    pub serial_number: String,
    /// `MAC`, unless scrubbed.
    pub mac: String,
    /// Counters from `CMD_GET_FREE_SIZES`.
    pub sizes: Sizes,
}

/// Counters and capacities.
#[derive(Debug, Clone, Serialize)]
#[allow(missing_docs, reason = "named after the device counters")]
pub struct Sizes {
    pub users: u32,
    pub fingers: u32,
    pub records: u32,
    pub cards: u32,
    pub users_capacity: u32,
    pub fingers_capacity: u32,
    pub records_capacity: u32,
    pub faces: Option<u32>,
}

/// One transport.
#[derive(Debug, Clone, Serialize)]
pub struct Probe {
    /// `tcp` or `udp`.
    pub transport: String,
    /// Chunking used for table reads.
    pub chunking: String,
    /// Connection error, if it failed.
    pub connect_error: Option<String>,
    /// The user table read.
    pub users: Option<TableRead>,
    /// The attendance table read.
    pub attendance: Option<TableRead>,
    /// Replies discarded for a mismatched reply ID.
    pub discarded_replies: u64,
    /// Reply-ID behaviour seen.
    pub reply_ids: String,
    /// Live events that arrived (acknowledged, not reported).
    pub live_events: usize,
    #[serde(skip)]
    tables: [Option<Bytes>; 2],
    #[serde(skip)]
    recording: Recording,
    #[serde(skip)]
    stream: Option<Result<Option<StreamHeaders>, String>>,
}

/// One table read.
#[derive(Debug, Clone, Serialize)]
pub struct TableRead {
    /// Records the device counts.
    pub count: u32,
    /// Table bytes received, length prefix included.
    pub bytes: Option<usize>,
    /// Bytes per record, if the table divides evenly.
    pub record_size: Option<usize>,
    /// The layout that size corresponds to, if known.
    pub layout: Option<String>,
    /// Records decoded and rejected with that layout.
    pub decoded: Option<usize>,
    /// Records rejected.
    pub rejected: Option<usize>,
    /// Seconds the read took.
    pub seconds: f64,
    /// The error, if the read failed.
    pub error: Option<String>,
    #[serde(skip)]
    timed_out: bool,
}

/// Whether a UDP probe read the same bytes as TCP.
#[derive(Debug, Clone, Serialize)]
pub struct Comparison {
    /// Chunking of the UDP probe.
    pub chunking: String,
    /// Users table identical, if both were read.
    pub users: Option<bool>,
    /// Attendance table identical, if both were read.
    pub attendance: Option<bool>,
}

/// The report, and capture files by name if asked for.
#[derive(Debug)]
pub struct Outcome {
    /// The report.
    pub report: Report,
    /// `(file name, capture)` pairs.
    pub captures: Vec<(String, Value)>,
}

/// The transports probed, with their chunking.
const MODES: [(Transport, &str); 2] = [(Transport::Tcp, "64 KiB"), (Transport::Udp, "single datagram")];

/// A read-only configuration for `opts`.
#[must_use]
pub fn config(opts: &Options, transport: Transport) -> ClientConfig {
    let mut config = ClientConfig::new(opts.host.clone())
        .port(opts.port)
        .transport(transport)
        .request_timeout(opts.timeout)
        .read_only(true);
    if let Some(pw) = opts.password {
        config = config.password(pw);
    }
    config
}

/// Run the survey.
///
/// # Errors
///
/// Only if the main connection or its information queries fail; probe
/// failures are recorded in the report.
pub async fn run(opts: &Options) -> zkteco_client::Result<Outcome> {
    let mut client = Client::connect(config(opts, opts.transport)).await?;
    let facts = client.device_facts().await?;
    let sizes = client.read_sizes().await?;
    let device = Device {
        connected_over: wire_name(client.transport()).into(),
        serial_number: identifier(&mut client, keys::SERIAL_NUMBER, opts).await?,
        mac: identifier(&mut client, keys::MAC, opts).await?,
        sizes: sizes_of(&sizes),
    };
    let main_wire = client.transport();
    if let Err(e) = client.disconnect().await {
        tracing::warn!(%e, "disconnect after the survey");
    }
    let mut probes = Vec::with_capacity(MODES.len());
    for (transport, label) in MODES {
        probes.push(probe(opts, transport, label, &sizes).await);
    }
    let captures = if opts.capture {
        captures(&probes, &facts)
    } else {
        Vec::new()
    };
    let live = match opts.probe_live {
        Some(listen) => Some(
            live_probe::run(
                live_probe::config(config(opts, main_transport(main_wire))),
                listen,
            )
            .await,
        ),
        None => None,
    };
    let stream = probes
        .iter()
        .find_map(|p| p.stream.clone())
        .unwrap_or_else(|| Err("no UDP connection".into()));
    let report = Report {
        row: row(&facts, &probes, main_wire),
        device,
        udp_stream_headers: stream.clone().ok().flatten(),
        udp_stream_note: match stream {
            Ok(Some(_)) => None,
            Ok(None) => Some("tables too small to stream (sent inline)".into()),
            Err(e) => Some(e),
        },
        udp_matches_tcp: compare(&probes),
        live,
        probes,
        tool: format!("zkctl {}", env!("CARGO_PKG_VERSION")),
    };
    Ok(Outcome { report, captures })
}

fn main_transport(wire: zkteco_proto::Transport) -> Transport {
    match wire {
        zkteco_proto::Transport::Tcp => Transport::Tcp,
        zkteco_proto::Transport::Udp => Transport::Udp,
    }
}

async fn identifier(client: &mut Client, key: &str, opts: &Options) -> zkteco_client::Result<String> {
    if !opts.include_identifiers {
        return Ok(SCRUBBED.into());
    }
    match client.option(key).await {
        Ok(v) => Ok(v),
        Err(e) if e.is_fatal() => Err(e),
        Err(_) => Ok(zkteco_client::compat::UNAVAILABLE.into()),
    }
}

/// Counters from `CMD_GET_FREE_SIZES` in report form.
#[must_use]
pub fn sizes_of(s: &DeviceSizes) -> Sizes {
    Sizes {
        users: s.users(),
        fingers: s.fingers(),
        records: s.records(),
        cards: s.cards(),
        users_capacity: s.users_capacity(),
        fingers_capacity: s.fingers_capacity(),
        records_capacity: s.records_capacity(),
        faces: s.faces(),
    }
}

fn wire_name(t: zkteco_proto::Transport) -> &'static str {
    match t {
        zkteco_proto::Transport::Tcp => "tcp",
        zkteco_proto::Transport::Udp => "udp",
    }
}

/// Read both tables in one mode under Strict reply IDs; if that times out,
/// once more under Lenient to tell "not echoed" from "unreachable".
async fn probe(opts: &Options, transport: Transport, label: &str, sizes: &DeviceSizes) -> Probe {
    let strict = config(opts, transport);
    let mut p = read_tables(strict.clone(), label, sizes).await;
    p.reply_ids = if p.connect_error.is_some() {
        "unknown (no connection)".into()
    } else if !timed_out(&p) {
        if p.discarded_replies == 0 {
            "echoed".into()
        } else {
            "mismatches discarded".into()
        }
    } else {
        let lenient = read_tables(strict.reply_id_policy(ReplyIdPolicy::Lenient), label, sizes).await;
        if lenient.connect_error.is_none() && !timed_out(&lenient) {
            let mut lenient = lenient;
            lenient.reply_ids = "not echoed (lenient required)".into();
            return lenient;
        }
        "unknown (timeouts under strict and lenient)".into()
    };
    p
}

fn timed_out(p: &Probe) -> bool {
    [&p.users, &p.attendance]
        .into_iter()
        .flatten()
        .any(|t| t.timed_out)
}

async fn read_tables(config: ClientConfig, label: &str, sizes: &DeviceSizes) -> Probe {
    let transport = match config.transport {
        Transport::Udp => "udp",
        _ => "tcp",
    };
    let mut p = Probe {
        transport: transport.into(),
        chunking: label.into(),
        connect_error: None,
        users: None,
        attendance: None,
        discarded_replies: 0,
        reply_ids: String::new(),
        live_events: 0,
        tables: [None, None],
        recording: Recording::default(),
        stream: None,
    };
    let mut client = match Client::connect(config).await {
        Ok(c) => c,
        Err(e) => {
            p.connect_error = Some(e.to_string());
            return p;
        }
    };
    client.start_recording();
    let (users, data) = read_table(&mut client, RawTable::Users, sizes.users()).await;
    (p.users, p.tables[0]) = (Some(users), data);
    let (att, data) = read_table(&mut client, RawTable::Attendance, sizes.records()).await;
    (p.attendance, p.tables[1]) = (Some(att), data);
    p.discarded_replies = client.discarded_replies();
    p.live_events = client.take_pending_events().len();
    p.recording = client.take_recording();
    if transport == "udp" {
        let bigger =
            if p.tables[1].as_ref().map_or(0, Bytes::len) > p.tables[0].as_ref().map_or(0, Bytes::len) {
                RawTable::Attendance
            } else {
                RawTable::Users
            };
        p.stream = Some(sample(&mut client, bigger).await);
    }
    if client.is_connected() {
        if let Err(e) = client.disconnect().await {
            tracing::warn!(%e, "disconnect after a probe");
        }
    }
    p
}

/// Sample one 16 KiB UDP stream of `which`; headers only.
async fn sample(client: &mut Client, which: RawTable) -> Result<Option<StreamHeaders>, String> {
    if !client.is_connected() {
        client.reconnect().await.map_err(|e| e.to_string())?;
    }
    match client.sample_udp_stream(which).await {
        Ok(s) => Ok(s.as_ref().map(streams::analyse)),
        Err(e) => Err(format!("sampling failed: {e}")),
    }
}

async fn read_table(client: &mut Client, which: RawTable, count: u32) -> (TableRead, Option<Bytes>) {
    let mut read = TableRead {
        count,
        bytes: None,
        record_size: None,
        layout: None,
        decoded: None,
        rejected: None,
        seconds: 0.0,
        error: None,
        timed_out: false,
    };
    if !client.is_connected() {
        if let Err(e) = client.reconnect().await {
            read.error = Some(e.to_string());
            return (read, None);
        }
    }
    let started = Instant::now();
    let result = client.raw_table(which).await;
    read.seconds = started.elapsed().as_secs_f64();
    let data = match result {
        Ok(d) => d,
        Err(e) => {
            read.timed_out = matches!(e, Error::Timeout { .. });
            read.error = Some(e.to_string());
            return (read, None);
        }
    };
    read.bytes = Some(data.len());
    let body = data.len().saturating_sub(4);
    let n = usize::try_from(count).unwrap_or(usize::MAX);
    if n > 0 && body.is_multiple_of(n) {
        read.record_size = Some(body / n);
    }
    describe_layout(&mut read, which, &data);
    (read, Some(data))
}

/// Name the layout for the record size and decode with it, counting only.
fn describe_layout(read: &mut TableRead, which: RawTable, data: &[u8]) {
    let Some(size) = read.record_size else {
        return;
    };
    let enc = TextEncoding::UTF8;
    let counts = match which {
        RawTable::Users => UserFormat::from_record_len(size).map(|f| {
            read.layout = Some(format!("{f:?}"));
            user::decode_table_as(data, f, enc).map(|b| (b.records.len(), b.rejected.len()))
        }),
        RawTable::Attendance => AttendanceFormat::from_record_len(size).map(|f| {
            read.layout = Some(format!("{f:?}"));
            attendance::decode_table_as(data, f, enc).map(|b| (b.records.len(), b.rejected.len()))
        }),
    };
    if let Some(Ok((decoded, rejected))) = counts {
        (read.decoded, read.rejected) = (Some(decoded), Some(rejected));
    }
}

fn row(facts: &DeviceFacts, probes: &[Probe], main: zkteco_proto::Transport) -> Row {
    let main_probe = probes.iter().find(|p| p.transport == wire_name(main));
    let size = |pick: fn(&Probe) -> &Option<TableRead>| {
        main_probe
            .into_iter()
            .chain(probes)
            .find_map(|p| pick(p).as_ref().and_then(|t| t.record_size))
            .map_or_else(|| "-".to_string(), |s| s.to_string())
    };
    let read_both = |p: &&Probe| {
        [&p.users, &p.attendance]
            .iter()
            .all(|t| t.as_ref().is_some_and(|t| t.error.is_none()))
    };
    let mut worked: Vec<&str> = probes
        .iter()
        .filter(read_both)
        .map(|p| p.transport.as_str())
        .collect();
    worked.dedup();
    Row {
        model: facts.model.clone(),
        firmware: facts.firmware.clone(),
        platform: facts.platform.clone(),
        user_ext_fmt: facts.user_ext_fmt.clone(),
        extend_fmt: facts.extend_fmt.clone(),
        user_record: size(|p| &p.users),
        attendance_record: size(|p| &p.attendance),
        reply_ids: main_probe.map_or_else(|| "unknown".into(), |p| p.reply_ids.clone()),
        transport: if worked.is_empty() {
            "-".into()
        } else {
            worked.join("+")
        },
        verified_by: String::new(),
    }
}

fn compare(probes: &[Probe]) -> Vec<Comparison> {
    let Some(tcp) = probes.iter().find(|p| p.transport == "tcp") else {
        return Vec::new();
    };
    probes
        .iter()
        .filter(|p| p.transport == "udp")
        .map(|udp| {
            let same = |i: usize| match (&tcp.tables[i], &udp.tables[i]) {
                (Some(a), Some(b)) => Some(a == b),
                _ => None,
            };
            Comparison {
                chunking: udp.chunking.clone(),
                users: same(0),
                attendance: same(1),
            }
        })
        .collect()
}

fn captures(probes: &[Probe], facts: &DeviceFacts) -> Vec<(String, Value)> {
    let layout = |t: &Option<TableRead>| t.as_ref().and_then(|t| t.record_size);
    probes
        .iter()
        .filter(|p| p.connect_error.is_none())
        .map(|p| {
            let layouts = Layouts {
                users: layout(&p.users).and_then(UserFormat::from_record_len),
                attendance: layout(&p.attendance).and_then(AttendanceFormat::from_record_len),
            };
            let meta = Meta {
                device: facts.model.clone(),
                firmware: facts.firmware.clone(),
                platform: facts.platform.clone(),
                transport: if p.transport == "udp" {
                    zkteco_proto::Transport::Udp
                } else {
                    zkteco_proto::Transport::Tcp
                },
                notes: format!(
                    "zkctl diagnose: users and attendance, {} {}; scrubbed",
                    p.transport, p.chunking
                ),
            };
            let name = format!("diagnose-{}-{}.json", p.transport, p.chunking.replace(' ', "-"));
            (name, capture::build(&p.recording, &meta, layouts))
        })
        .collect()
}
