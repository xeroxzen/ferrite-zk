//! Fingerprint enrolment (`Client::enroll_user`) against the mock device.

mod support;

use std::time::Duration;

use support::{users, DeviceState, MockDevice};
use zkteco_client::{Client, EnrollOptions, EnrollOutcome, EnrollReport, Error, EventFlags, UserId};
use zkteco_proto::Command;

const FINGER: EventFlags = EventFlags::ENROLL_FINGER;

fn code(c: u16) -> (EventFlags, Vec<u8>) {
    (FINGER, c.to_le_bytes().to_vec())
}

/// Three scans the way the reference flow expects them: a report, then
/// "scan accepted", three times; then the result.
fn three_scans(result: (EventFlags, Vec<u8>)) -> Vec<(EventFlags, Vec<u8>)> {
    let mut script = Vec::new();
    for _ in 0..3 {
        script.push(code(1));
        script.push(code(0x64));
    }
    script.push(result);
    script
}

fn fast() -> EnrollOptions {
    EnrollOptions::default().report_timeout(Duration::from_millis(200))
}

async fn device(script: Vec<(EventFlags, Vec<u8>)>) -> MockDevice {
    MockDevice::start(DeviceState {
        users: users(2),
        enroll_script: script,
        ..DeviceState::default()
    })
    .await
}

async fn enroll(dev: &MockDevice) -> (zkteco_client::Result<EnrollOutcome>, Vec<EnrollReport>) {
    let mut client = Client::connect(dev.config()).await.unwrap();
    let mut seen = Vec::new();
    let outcome = client
        .enroll_user(&UserId("1001".into()), 3, fast(), |r| seen.push(r))
        .await;
    (outcome, seen)
}

#[tokio::test]
async fn stored_after_three_scans() {
    let stored = (
        FINGER,
        [0u16, 512, 5].iter().flat_map(|w| w.to_le_bytes()).collect(),
    );
    let dev = device(three_scans(stored)).await;
    let (outcome, seen) = enroll(&dev).await;
    assert_eq!(
        outcome.unwrap(),
        EnrollOutcome::Stored {
            size: Some(512),
            position: Some(5),
            scans: 3
        }
    );
    assert_eq!(seen.len(), 7);
    assert_eq!(seen[1], EnrollReport::ScanAccepted);
    let request = dev.state().enroll_requests[0].clone();
    assert_eq!(request.len(), 26, "TCP: 24-byte user ID, finger, 1");
    assert_eq!(&request[..4], b"1001");
    assert_eq!(request[24..], [3, 1]);
    assert_eq!(dev.count(Command::AckOk), 7, "every report acknowledged");
    assert_eq!(
        dev.commands()[1..3],
        [Command::CancelCapture, Command::StartEnroll]
    );
    let n = dev.commands().len();
    assert_eq!(
        dev.commands()[n - 3..],
        [Command::RegEvent, Command::CancelCapture, Command::StartVerify]
    );
    assert_eq!(dev.state().event_masks, [0]);
}

#[tokio::test]
async fn terminal_reports() {
    for (c, expected) in [
        (5, EnrollOutcome::Duplicate),
        (4, EnrollOutcome::Failed),
        (6, EnrollOutcome::TimedOut),
    ] {
        let dev = device(vec![code(1), code(0x64), code(c)]).await;
        assert_eq!(enroll(&dev).await.0.unwrap(), expected, "code {c}");
        assert_eq!(dev.commands().last(), Some(&Command::StartVerify));
    }
}

#[tokio::test]
async fn stored_before_any_scan_is_unexpected() {
    let dev = device(vec![code(0)]).await;
    let outcome = enroll(&dev).await.0.unwrap();
    assert_eq!(
        outcome,
        EnrollOutcome::Unexpected {
            report: EnrollReport::Stored {
                size: None,
                position: None
            },
            scans: 0
        }
    );
}

#[tokio::test]
async fn silence_is_no_report_and_still_cleans_up() {
    let dev = device(vec![code(0x64)]).await;
    let outcome = enroll(&dev).await.0.unwrap();
    assert_eq!(outcome, EnrollOutcome::NoReport { scans: 1 });
    assert_eq!(dev.commands().last(), Some(&Command::StartVerify));
}

#[tokio::test]
async fn attendance_during_enrolment_is_queued() {
    let dev = device(Vec::new()).await;
    let punch = dev.state().event_packet();
    let mut script = three_scans(code(0));
    script.insert(2, (EventFlags::ATTLOG, punch[8..].to_vec()));
    dev.state().enroll_script = script;
    let mut client = Client::connect(dev.config()).await.unwrap();
    let mut seen = 0;
    let outcome = client
        .enroll_user(&UserId("1001".into()), 0, fast(), |_| seen += 1)
        .await
        .unwrap();
    assert!(
        matches!(outcome, EnrollOutcome::Stored { scans: 3, .. }),
        "{outcome:?}"
    );
    assert_eq!(seen, 7, "the punch is not a report");
    assert_eq!(client.take_pending_events().len(), 1);
}

#[tokio::test]
async fn finger_index_is_checked_before_sending() {
    let dev = device(Vec::new()).await;
    let mut client = Client::connect(dev.config()).await.unwrap();
    let err = client
        .enroll_user(&UserId("1001".into()), 10, fast(), |_| {})
        .await
        .unwrap_err();
    assert!(matches!(err, Error::InvalidArgument(_)), "{err:?}");
    assert_eq!(dev.commands(), [Command::Connect]);
}

#[tokio::test]
async fn udp_sends_a_numeric_user_id() {
    let dev = device(three_scans(code(0))).await;
    let mut client = Client::connect(dev.config_udp()).await.unwrap();
    let outcome = client
        .enroll_user(&UserId("1001".into()), 2, fast(), |_| {})
        .await
        .unwrap();
    assert!(
        matches!(outcome, EnrollOutcome::Stored { scans: 3, .. }),
        "{outcome:?}"
    );
    let mut expected = 1001u32.to_le_bytes().to_vec();
    expected.push(2);
    assert_eq!(dev.state().enroll_requests, [expected]);

    let err = client
        .enroll_user(&UserId("A7".into()), 2, fast(), |_| {})
        .await
        .unwrap_err();
    assert!(matches!(err, Error::InvalidArgument(_)), "{err:?}");
}

#[tokio::test]
async fn read_only_client_cannot_enroll() {
    let dev = device(Vec::new()).await;
    let mut client = Client::connect(dev.config().read_only(true)).await.unwrap();
    let err = client
        .enroll_user(&UserId("1001".into()), 0, fast(), |_| {})
        .await
        .unwrap_err();
    assert!(matches!(err, Error::ReadOnly(Command::CancelCapture)), "{err:?}");
}
