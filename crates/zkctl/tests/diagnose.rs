//! `zkctl diagnose` against the mock device from `zkteco-client`'s tests.

#[path = "../../zkteco-client/tests/support/mod.rs"]
mod support;

use std::time::Duration;

use serde_json::Value;
use support::{punches, users, DeviceState, MockDevice};
use zkctl::diagnose::{self, Options, SCRUBBED};
use zkteco_client::{Transport, READ_ONLY_COMMANDS};
use zkteco_proto::framing::TcpFrameDecoder;
use zkteco_proto::packet::{self, Packet};
use zkteco_proto::{ChecksumPolicy, Command};

fn options(dev: &MockDevice) -> Options {
    Options {
        host: dev.addr.ip().to_string(),
        port: dev.addr.port(),
        password: Some(4242),
        transport: Transport::Auto,
        timeout: Duration::from_millis(500),
        include_identifiers: false,
        capture: true,
    }
}

async fn device() -> MockDevice {
    MockDevice::start(DeviceState {
        password: Some(4242),
        users: users(40),
        attendance: punches(120),
        ..DeviceState::default()
    })
    .await
}

#[tokio::test]
async fn report_is_a_compatibility_row() {
    let dev = device().await;
    let outcome = diagnose::run(&options(&dev)).await.unwrap();
    let r = &outcome.report;
    assert_eq!(r.row.model, "Mock");
    assert_eq!(r.row.firmware, "Ver 6.60 Mock");
    assert_eq!(r.row.platform, "MOCK_TFT");
    assert_eq!(r.row.user_record, "72");
    assert_eq!(r.row.attendance_record, "40");
    assert_eq!(r.row.reply_ids, "echoed");
    assert_eq!(r.row.transport, "tcp+udp");
    assert!(r.row.verified_by.is_empty());
    assert_eq!(r.device.connected_over, "tcp");
    assert_eq!(
        (r.device.serial_number.as_str(), r.device.mac.as_str()),
        (SCRUBBED, SCRUBBED)
    );
    assert_eq!(r.probes.len(), 3);
    for p in &r.probes {
        let users = p.users.as_ref().unwrap();
        assert_eq!((users.decoded, users.rejected), (Some(40), Some(0)), "{p:?}");
    }
    assert!(r
        .udp_matches_tcp
        .iter()
        .all(|c| c.users == Some(true) && c.attendance == Some(true)));
    let headers = r.udp_stream_headers.as_ref().unwrap();
    assert!(
        headers.session_ids.starts_with("sequence"),
        "the mock numbers its packets: {headers:?}"
    );
    let json = serde_json::to_value(r).unwrap();
    assert!(!json.to_string().contains("MOCK0000000001"), "serial leaked");
}

#[tokio::test]
async fn sends_only_allowlisted_commands() {
    let dev = device().await;
    diagnose::run(&options(&dev)).await.unwrap();
    let commands = dev.commands();
    assert!(commands.contains(&Command::ReadBuffer));
    for c in commands {
        assert!(READ_ONLY_COMMANDS.contains(&c), "{c:?} sent");
    }
}

#[tokio::test]
async fn identifiers_only_on_request() {
    let dev = device().await;
    let opts = Options {
        include_identifiers: true,
        capture: false,
        ..options(&dev)
    };
    let outcome = diagnose::run(&opts).await.unwrap();
    assert_eq!(outcome.report.device.serial_number, "MOCK0000000001");
    assert!(outcome.captures.is_empty());
}

/// Every packet of a capture, decoded with checksums enforced.
fn packets(capture: &Value) -> Vec<Packet> {
    let tcp = capture["transport"] == "tcp";
    let mut decoders = [TcpFrameDecoder::default(), TcpFrameDecoder::default()];
    let mut out = Vec::new();
    for e in capture["packets"].as_array().unwrap() {
        let bytes = hex::decode(e["hex"].as_str().unwrap()).unwrap();
        if tcp {
            let d = &mut decoders[usize::from(e["direction"] == "device_to_host")];
            d.extend(&bytes);
            while let Some(p) = d.next_packet().unwrap() {
                out.push(packet::decode(p, ChecksumPolicy::Reject).unwrap());
            }
        } else {
            out.push(packet::decode(bytes.into(), ChecksumPolicy::Reject).unwrap());
        }
    }
    out
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[tokio::test]
async fn captures_are_scrubbed_and_valid() {
    let dev = device().await;
    let outcome = diagnose::run(&options(&dev)).await.unwrap();
    assert_eq!(outcome.captures.len(), 3);
    let originals = dev.state().users.clone();
    for (name, capture) in &outcome.captures {
        assert_eq!(capture["platform"], "MOCK_TFT", "{name}");
        let all = packets(capture);
        assert!(
            all.iter().any(|p| p.header.command == Command::ReadBuffer),
            "{name}"
        );
        let wire: Vec<u8> = all.iter().flat_map(|p| p.payload.to_vec()).collect();
        for u in &originals {
            assert!(
                !contains(&wire, u.user_id.as_bytes()),
                "{name}: user ID {} leaked",
                u.user_id
            );
            assert!(
                !contains(&wire, u.name.as_bytes()),
                "{name}: name {} leaked",
                u.name
            );
        }
        assert!(!contains(&wire, b"MOCK0000000001"), "{name}: serial leaked");
        // Recording starts after the handshake; an Auth packet appears only
        // after a reconnect, and must then be zeroed (capture unit tests).
        assert!(
            all.iter()
                .filter(|p| p.header.command == Command::Auth)
                .all(|p| p.payload.iter().all(|&b| b == 0)),
            "{name}: comm key kept"
        );
        assert!(contains(&wire, b"User1\0"), "{name}: synthetic names present");
    }
}
