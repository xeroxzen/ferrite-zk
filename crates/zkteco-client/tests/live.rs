//! Live capture (`Client::live_capture`) against the mock device.

mod support;

use std::time::Duration;

use futures_util::StreamExt;
use support::{event_with, users, Delivery, DeviceState, MockDevice};
use zkteco_client::{
    CaptureOptions, CapturedEvent, Client, Error, EventFlags, LiveCapture, LiveEvent, ReconnectPolicy,
    Resume, Source, Uid, UserId, UserIdSource,
};
use zkteco_proto::records::attendance::{Attendance, AttendanceStatus, Punch};
use zkteco_proto::records::event::{encode_record, EventRecordFormat};
use zkteco_proto::{Command, TextEncoding};

/// No heartbeat, so tests see only the packets they cause.
fn quiet() -> CaptureOptions {
    CaptureOptions::default().heartbeat(None)
}

fn reconnecting(max_attempts: u32) -> CaptureOptions {
    quiet().reconnect(
        ReconnectPolicy::default()
            .max_attempts(max_attempts)
            .initial_backoff(Duration::from_millis(10))
            .max_backoff(Duration::from_millis(40)),
    )
}

async fn next(capture: &mut LiveCapture) -> Option<zkteco_client::Result<CapturedEvent>> {
    tokio::time::timeout(Duration::from_secs(5), capture.next())
        .await
        .expect("the stream produced an item in time")
}

fn user_id(captured: &CapturedEvent) -> String {
    event_user_id(&captured.event)
}

fn event_user_id(event: &LiveEvent) -> String {
    match event {
        LiveEvent::Attendance(a) => a.user_id.as_ref().expect("user ID").as_str().to_string(),
        LiveEvent::Raw { .. } => panic!("expected attendance, got {event:?}"),
    }
}

#[tokio::test]
async fn subscribes_streams_and_stops() {
    let dev = MockDevice::start(DeviceState::default()).await;
    let client = Client::connect(dev.config()).await.unwrap();
    let mut capture = client.live_capture(quiet()).await.unwrap();
    assert_eq!(
        dev.commands()[1..],
        [Command::CancelCapture, Command::StartVerify, Command::RegEvent]
    );
    assert_eq!(dev.state().event_masks, [EventFlags::ATTLOG.bits()]);

    dev.push_events(3);
    for expected in ["1", "2", "3"] {
        assert_eq!(user_id(&next(&mut capture).await.unwrap().unwrap()), expected);
    }
    let (mut client, result) = capture.stop().await;
    result.unwrap();
    assert_eq!(dev.state().event_masks, [EventFlags::ATTLOG.bits(), 0]);
    assert_eq!(dev.count(Command::AckOk), 3, "every event acknowledged");
    assert_eq!(client.take_pending_events(), Vec::new());
    client.firmware_version().await.unwrap();
}

#[tokio::test]
async fn options_choose_the_mask_and_skip_preparation() {
    let dev = MockDevice::start(DeviceState::default()).await;
    let client = Client::connect(dev.config()).await.unwrap();
    let opts = quiet()
        .events(EventFlags::ATTLOG | EventFlags::VERIFY)
        .prepare(false);
    let capture = client.live_capture(opts).await.unwrap();
    assert_eq!(dev.commands()[1..], [Command::RegEvent]);
    assert_eq!(
        dev.state().event_masks,
        [(EventFlags::ATTLOG | EventFlags::VERIFY).bits()]
    );
    capture.stop().await.1.unwrap();
}

#[tokio::test]
async fn coalesced_and_split_events_all_arrive() {
    for delivery in [Delivery::Coalesced, Delivery::Split(5)] {
        let dev = MockDevice::start(DeviceState::default()).await;
        let mut capture = LiveCapture::connect(dev.config(), quiet()).await.unwrap();
        dev.state().faults.delivery = delivery;
        dev.push_events(4);
        let mut ids = Vec::new();
        for _ in 0..4 {
            ids.push(user_id(&next(&mut capture).await.unwrap().unwrap()));
        }
        assert_eq!(ids, ["1", "2", "3", "4"], "{delivery:?}");
    }
}

#[tokio::test]
async fn events_queued_before_the_capture_come_first() {
    let dev = MockDevice::start(DeviceState::default()).await;
    dev.state().faults.event_before = Some(Command::GetVersion);
    let mut client = Client::connect(dev.config()).await.unwrap();
    client.firmware_version().await.unwrap();
    dev.state().faults.event_before = None;
    let mut capture = client.live_capture(quiet()).await.unwrap();
    dev.push_events(1);
    assert_eq!(user_id(&next(&mut capture).await.unwrap().unwrap()), "1");
    assert_eq!(user_id(&next(&mut capture).await.unwrap().unwrap()), "2");
}

#[tokio::test]
async fn unexpected_payloads_are_raw() {
    let dev = MockDevice::start(DeviceState::default()).await;
    let mut capture = LiveCapture::connect(dev.config(), quiet()).await.unwrap();
    let odd = event_with(EventFlags::ATTLOG, &[1, 2, 3]);
    let alarm = event_with(EventFlags::ALARM, &[55, 0, 0, 0]);
    dev.push_packets(vec![odd, alarm]);
    let first = next(&mut capture).await.unwrap().unwrap().event;
    assert!(
        matches!(&first, LiveEvent::Raw { flag, payload } if *flag == EventFlags::ATTLOG && payload[..] == [1, 2, 3]),
        "{first:?}"
    );
    let second = next(&mut capture).await.unwrap().unwrap().event;
    assert!(
        matches!(&second, LiveEvent::Raw { flag, .. } if *flag == EventFlags::ALARM),
        "{second:?}"
    );
}

#[tokio::test]
async fn resolves_users_when_asked() {
    let dev = MockDevice::start(DeviceState {
        users: users(3),
        ..DeviceState::default()
    })
    .await;
    let opts = quiet().resolve_users(true);
    let mut capture = LiveCapture::connect(dev.config(), opts).await.unwrap();
    let mut punch = Attendance::new(dev.state().time, AttendanceStatus::Fingerprint, Punch::CheckOut);
    punch.user_id = Some(UserId("1002".into()));
    let payload = encode_record(&punch, EventRecordFormat::Numeric12, TextEncoding::UTF8).unwrap();
    let packet = event_with(EventFlags::ATTLOG, &payload);
    dev.push_packets(vec![packet]);
    let captured = next(&mut capture).await.unwrap().unwrap();
    assert_eq!(
        captured.user_id_source,
        Some(UserIdSource::Record),
        "the event had it"
    );
    let LiveEvent::Attendance(a) = captured.event else {
        panic!("expected attendance");
    };
    assert_eq!(a.uid, Some(Uid(2)));
    assert_eq!(a.punch, Punch::CheckOut);
}

#[tokio::test]
async fn next_is_cancel_safe() {
    let dev = MockDevice::start(DeviceState::default()).await;
    let mut capture = LiveCapture::connect(dev.config(), quiet()).await.unwrap();
    let mut ids = Vec::new();
    // Each `next` is abandoned after 2 ms, mostly mid-step; none may lose
    // an event.
    for round in 0..400 {
        if round % 20 == 0 && round < 200 {
            dev.push_events(2);
        }
        if let Ok(item) = tokio::time::timeout(Duration::from_millis(2), capture.next()).await {
            ids.push(user_id(&item.unwrap().unwrap()));
        }
        if ids.len() == 20 {
            break;
        }
    }
    let expected: Vec<String> = (1..=20).map(|i| i.to_string()).collect();
    assert_eq!(ids, expected);
}

#[tokio::test]
async fn stop_keeps_an_event_received_by_the_step_in_flight() {
    let dev = MockDevice::start(DeviceState::default()).await;
    let mut capture = LiveCapture::connect(dev.config(), quiet()).await.unwrap();
    assert!(tokio::time::timeout(Duration::from_millis(5), capture.next())
        .await
        .is_err());
    dev.push_events(2);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let (mut client, result) = capture.stop().await;
    result.unwrap();
    let kept: Vec<String> = client.take_pending_events().iter().map(event_user_id).collect();
    assert_eq!(kept, ["1", "2"]);
}

#[tokio::test]
async fn works_as_a_stream() {
    let dev = MockDevice::start(DeviceState::default()).await;
    let capture = LiveCapture::connect(dev.config(), quiet()).await.unwrap();
    dev.push_events(3);
    let ids: Vec<String> = tokio::time::timeout(
        Duration::from_secs(5),
        capture.take(3).map(|e| user_id(&e.unwrap())).collect(),
    )
    .await
    .unwrap();
    assert_eq!(ids, ["1", "2", "3"]);
}

#[tokio::test]
async fn heartbeat_checks_an_idle_device() {
    let dev = MockDevice::start(DeviceState::default()).await;
    let opts = quiet().heartbeat(Some(Duration::from_millis(40)));
    let mut capture = LiveCapture::connect(dev.config(), opts).await.unwrap();
    assert!(tokio::time::timeout(Duration::from_millis(300), capture.next())
        .await
        .is_err());
    assert!(dev.count(Command::GetTime) >= 2, "{:?}", dev.commands());
}

#[tokio::test]
async fn unanswered_heartbeat_ends_the_stream_without_reconnect() {
    let dev = MockDevice::start(DeviceState::default()).await;
    dev.state().faults.drop_reply_once = Some(Command::GetTime);
    let opts = quiet()
        .heartbeat(Some(Duration::from_millis(20)))
        .heartbeat_misses(1);
    let mut capture = LiveCapture::connect(dev.config(), opts).await.unwrap();
    let err = next(&mut capture).await.unwrap().unwrap_err();
    assert!(
        matches!(
            err,
            Error::Timeout {
                command: Command::GetTime,
                ..
            }
        ),
        "{err:?}"
    );
    assert!(next(&mut capture).await.is_none());
    assert!(futures_util::stream::FusedStream::is_terminated(&capture));
    let (client, result) = capture.stop().await;
    result.unwrap();
    assert!(!client.is_connected());
}

#[tokio::test]
async fn reconnects_and_resubscribes() {
    let dev = MockDevice::start(DeviceState::default()).await;
    let mut capture = LiveCapture::connect(dev.config(), reconnecting(3)).await.unwrap();
    assert!(capture.stats().connected);
    dev.drop_connection();
    assert!(
        next(&mut capture).await.unwrap().is_err(),
        "the lost connection is reported"
    );
    assert!(!capture.stats().connected, "known lost when reported");
    // The reconnect happens while the stream is polled.
    let mut pushed = false;
    let mut event = None;
    for _ in 0..200 {
        if !pushed && dev.state().event_masks.len() == 2 {
            dev.push_events(1);
            pushed = true;
        }
        if let Ok(item) = tokio::time::timeout(Duration::from_millis(10), capture.next()).await {
            event = item;
            break;
        }
    }
    assert_eq!(user_id(&event.unwrap().unwrap()), "1");
    let stats = capture.stats();
    assert_eq!(
        (stats.connected, stats.reconnects, stats.reconnect_failures),
        (true, 1, 0)
    );
    assert_eq!(dev.state().connections, 2);
    assert_eq!(dev.count(Command::CancelCapture), 2, "preparation repeated");
}

#[tokio::test]
async fn gives_up_after_the_reconnect_limit() {
    let dev = MockDevice::start(DeviceState::default()).await;
    let mut capture = LiveCapture::connect(dev.config(), reconnecting(2)).await.unwrap();
    dev.state().faults.close_on = Some(Command::Connect);
    dev.drop_connection();
    let mut errors = 0;
    let mut failures = Vec::new();
    for _ in 0..10 {
        match next(&mut capture).await {
            Some(item) => {
                item.unwrap_err();
                errors += 1;
                failures.push(capture.stats().reconnect_failures);
            }
            None => break,
        }
    }
    assert_eq!(errors, 3, "the lost connection, then two failed reconnects");
    assert_eq!(failures, [0, 1, 2], "counted per failed attempt");
    assert!(!capture.stats().connected);
    assert!(next(&mut capture).await.is_none(), "stays ended");
}

#[tokio::test]
async fn rejected_password_on_reconnect_ends_the_stream() {
    let dev = MockDevice::start(DeviceState::default()).await;
    let mut capture = LiveCapture::connect(dev.config(), reconnecting(5)).await.unwrap();
    dev.state().password = Some(77);
    dev.drop_connection();
    next(&mut capture).await.unwrap().unwrap_err();
    let err = next(&mut capture).await.unwrap().unwrap_err();
    assert!(matches!(err, Error::Unauthorized), "{err:?}");
    assert!(next(&mut capture).await.is_none());
}

#[tokio::test]
async fn enables_a_disabled_device_and_restores_it() {
    let dev = MockDevice::start(DeviceState::default()).await;
    let mut client = Client::connect(dev.config()).await.unwrap();
    client.disable_device().await.unwrap();
    let capture = client.live_capture(quiet()).await.unwrap();
    assert_eq!(dev.count(Command::EnableDevice), 1);
    capture.stop().await.1.unwrap();
    assert_eq!(dev.commands().last(), Some(&Command::DisableDevice));
}

#[tokio::test]
async fn leaves_an_enabled_device_alone() {
    let dev = MockDevice::start(DeviceState::default()).await;
    let capture = LiveCapture::connect(dev.config(), quiet()).await.unwrap();
    capture.stop().await.1.unwrap();
    assert_eq!(dev.count(Command::EnableDevice), 0);
    assert_eq!(dev.count(Command::DisableDevice), 0);
}

#[tokio::test]
async fn works_over_udp() {
    let dev = MockDevice::start(DeviceState::default()).await;
    let mut capture = LiveCapture::connect(dev.config_udp(), quiet()).await.unwrap();
    dev.push_events(2);
    assert_eq!(user_id(&next(&mut capture).await.unwrap().unwrap()), "1");
    assert_eq!(user_id(&next(&mut capture).await.unwrap().unwrap()), "2");
    capture.stop().await.1.unwrap();
    assert_eq!(dev.state().event_masks, [EventFlags::ATTLOG.bits(), 0]);
}

#[tokio::test]
async fn read_only_client_cannot_subscribe() {
    let dev = MockDevice::start(DeviceState::default()).await;
    let client = Client::connect(dev.config().read_only(true)).await.unwrap();
    let err = client.live_capture(quiet()).await.unwrap_err();
    assert!(matches!(err, Error::ReadOnly(Command::CancelCapture)), "{err:?}");
    assert_eq!(dev.count(Command::RegEvent), 0);
}

fn backfilling() -> CaptureOptions {
    reconnecting(3).backfill(true)
}

/// Poll the stream until it yields, allowing time for reconnects.
async fn next_item(capture: &mut LiveCapture) -> zkteco_client::Result<CapturedEvent> {
    next(capture).await.expect("stream still open")
}

/// A punch by user ID `n` at device time `at`, as the mock records it.
fn punch_by(at: chrono::NaiveDateTime, n: u16) -> Attendance {
    let mut record = Attendance::new(at, AttendanceStatus::Fingerprint, Punch::CheckIn);
    record.uid = Some(Uid(n));
    record.user_id = Some(UserId(n.to_string()));
    record
}

#[tokio::test]
async fn resume_backfills_what_was_missed_before_going_live() {
    let dev = MockDevice::start(DeviceState::default()).await;
    // Punches 1 and 2 were delivered by an earlier capture; 3 and 4 were
    // stored while nothing was listening. All share one device time.
    dev.record_offline(4);
    let at = dev.state().time;
    let opts = backfilling().resume(Resume::new(at, vec![punch_by(at, 1), punch_by(at, 2)]));
    let mut capture = LiveCapture::connect(dev.config(), opts).await.unwrap();
    dev.push_events(1);
    let mut got = Vec::new();
    for _ in 0..3 {
        let e = next_item(&mut capture).await.unwrap();
        got.push((user_id(&e), e.source));
    }
    let (b, l) = (Source::Backfill, Source::Live);
    assert_eq!(got, [("3".into(), b), ("4".into(), b), ("5".into(), l)]);
    assert_eq!(capture.stats().backfilled, 2);
    assert_eq!(dev.count(Command::PrepareBuffer), 1, "the log was read once");
    let (_, result) = capture.stop().await;
    result.unwrap();
}

#[tokio::test]
async fn resume_without_backfill_catches_nothing_up() {
    let dev = MockDevice::start(DeviceState::default()).await;
    dev.record_offline(2);
    let at = dev.state().time;
    let opts = reconnecting(3).resume(Resume::new(at, Vec::new()));
    let mut capture = LiveCapture::connect(dev.config(), opts).await.unwrap();
    dev.push_events(1);
    assert_eq!(user_id(&next_item(&mut capture).await.unwrap()), "3");
    assert_eq!(dev.count(Command::PrepareBuffer), 0, "the log was not read");
}

#[tokio::test]
async fn events_carry_device_and_host_times() {
    let dev = MockDevice::start(DeviceState::default()).await;
    let mut capture = LiveCapture::connect(dev.config(), quiet()).await.unwrap();
    let before = (std::time::SystemTime::now(), std::time::Instant::now());
    dev.push_events(2);
    let first = next_item(&mut capture).await.unwrap();
    let second = next_item(&mut capture).await.unwrap();
    assert_eq!(first.source, Source::Live);
    assert_eq!(first.device_time, Some(dev.state().time));
    assert!(first.received_at >= before.0);
    assert!(first.received_instant >= before.1);
    assert!(second.received_instant >= first.received_instant);
    assert_eq!(first.user_id_source, Some(UserIdSource::Record));
    let raw = event_with(EventFlags::ALARM, &[55, 0, 0, 0]);
    dev.push_packets(vec![raw]);
    let raw = next_item(&mut capture).await.unwrap();
    assert_eq!((raw.device_time, raw.user_id_source), (None, None));
}

#[tokio::test]
async fn a_missed_heartbeat_is_tolerated() {
    let dev = MockDevice::start(DeviceState::default()).await;
    dev.state().faults.drop_reply_once = Some(Command::GetTime);
    let opts = quiet().heartbeat(Some(Duration::from_millis(20)));
    let mut capture = LiveCapture::connect(dev.config(), opts).await.unwrap();
    // Long enough for the missed heartbeat (500 ms timeout) and another.
    assert!(tokio::time::timeout(Duration::from_millis(800), capture.next())
        .await
        .is_err());
    dev.push_events(1);
    assert_eq!(user_id(&next_item(&mut capture).await.unwrap()), "1");
    let stats = capture.stats();
    assert_eq!(stats.heartbeats_missed, 1);
    assert!(stats.heartbeats_answered >= 1, "{stats:?}");
    let (client, result) = capture.stop().await;
    result.unwrap();
    assert!(client.is_connected());
}

#[tokio::test]
async fn heartbeats_missed_in_a_row_end_the_connection() {
    let dev = MockDevice::start(DeviceState::default()).await;
    dev.state().faults.drop_replies = Some(Command::GetTime);
    let opts = quiet()
        .heartbeat(Some(Duration::from_millis(20)))
        .heartbeat_misses(2);
    let mut capture = LiveCapture::connect(dev.config(), opts).await.unwrap();
    let err = next_item(&mut capture).await.unwrap_err();
    assert!(
        matches!(
            err,
            Error::Timeout {
                command: Command::GetTime,
                ..
            }
        ),
        "{err:?}"
    );
    assert_eq!(capture.stats().heartbeats_missed, 2);
    assert!(next(&mut capture).await.is_none());
}

#[tokio::test]
async fn reconnect_backfills_missed_punches_once() {
    let dev = MockDevice::start(DeviceState::default()).await;
    let mut capture = LiveCapture::connect(dev.config(), backfilling()).await.unwrap();
    dev.push_events(2);
    for id in ["1", "2"] {
        let e = next_item(&mut capture).await.unwrap();
        assert_eq!((user_id(&e), e.source), (id.to_string(), Source::Live));
    }
    dev.drop_connection();
    dev.record_offline(2);
    next_item(&mut capture).await.unwrap_err();
    for id in ["3", "4"] {
        let e = next_item(&mut capture).await.unwrap();
        assert_eq!((user_id(&e), e.source), (id.to_string(), Source::Backfill));
        let stats = capture.stats();
        assert_eq!(
            (stats.connected, stats.backfilled),
            (true, 2),
            "connected only once the whole backfill is counted"
        );
    }
    // The device repeats punch 4 live (as if its acknowledgement was lost),
    // then sends a new one.
    let repeat = dev.state().event_packet_for(4);
    dev.push_packets(vec![repeat]);
    dev.push_events(1);
    let e = next_item(&mut capture).await.unwrap();
    assert_eq!((user_id(&e), e.source), ("5".to_string(), Source::Live));
    let stats = capture.stats();
    assert_eq!((stats.events, stats.backfilled, stats.reconnects), (5, 2, 1));
    assert_eq!(stats.duplicates, 3, "punches 1 and 2 in the log, punch 4 resent");
}

#[tokio::test]
async fn stop_keeps_undelivered_backfill() {
    let dev = MockDevice::start(DeviceState::default()).await;
    let mut capture = LiveCapture::connect(dev.config(), backfilling()).await.unwrap();
    dev.drop_connection();
    dev.record_offline(3);
    next_item(&mut capture).await.unwrap_err();
    assert_eq!(user_id(&next_item(&mut capture).await.unwrap()), "1");
    let (mut client, result) = capture.stop().await;
    result.unwrap();
    let kept: Vec<(String, Source)> = client
        .take_pending_captured()
        .iter()
        .map(|c| (event_user_id(&c.event), c.source))
        .collect();
    let backfill = |u: &str| (u.to_owned(), Source::Backfill);
    assert_eq!(kept, [backfill("2"), backfill("3")]);
}

#[tokio::test]
async fn read_only_client_may_subscribe_when_allowed() {
    let dev = MockDevice::start(DeviceState::default()).await;
    let config = dev.config().read_only(true).allow_event_subscription(true);
    let opts = quiet().prepare(false);
    let mut capture = LiveCapture::connect(config.clone(), opts).await.unwrap();
    dev.push_events(1);
    assert_eq!(user_id(&next_item(&mut capture).await.unwrap()), "1");
    capture.stop().await.1.unwrap();
    let err = LiveCapture::connect(config, quiet()).await.unwrap_err();
    assert!(matches!(err, Error::ReadOnly(Command::CancelCapture)), "{err:?}");
}
