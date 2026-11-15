//! Runs every example's `run` function against the mock device, so the
//! examples stay correct. Each example's `main` reads real settings and is
//! not called here.

mod support;

#[allow(
    dead_code,
    unreachable_pub,
    reason = "main is for cargo run --example; pub is for readers"
)]
#[path = "../examples/demo_device.rs"]
mod demo_device;
#[allow(
    dead_code,
    unreachable_pub,
    reason = "main is for cargo run --example; pub is for readers"
)]
#[path = "../examples/list_users.rs"]
mod list_users;
#[allow(
    dead_code,
    unreachable_pub,
    reason = "main is for cargo run --example; pub is for readers"
)]
#[path = "../examples/live_with_shutdown.rs"]
mod live_with_shutdown;
#[allow(
    dead_code,
    unreachable_pub,
    reason = "main is for cargo run --example; pub is for readers"
)]
#[path = "../examples/multi_device_live.rs"]
mod multi_device_live;
#[allow(
    dead_code,
    unreachable_pub,
    reason = "main is for cargo run --example; pub is for readers"
)]
#[path = "../examples/pull_attendance_since.rs"]
mod pull_attendance_since;

use std::time::Duration;

use chrono::NaiveDate;
use support::{punches, users, DeviceState, MockDevice};
use zkteco_client::{CaptureOptions, CapturedEvent, LiveCapture, LiveEvent, ReconnectPolicy, Source};

async fn wait_subscribed(dev: &MockDevice) {
    for _ in 0..500 {
        if !dev.state().event_masks.is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("never subscribed");
}

#[tokio::test]
async fn list_users_example() {
    let dev = MockDevice::start(DeviceState {
        users: users(3),
        ..DeviceState::default()
    })
    .await;
    let got = list_users::run(dev.config()).await.unwrap();
    assert_eq!(got, dev.state().users);
}

#[tokio::test]
async fn pull_attendance_since_example() {
    let dev = MockDevice::start(DeviceState {
        users: users(3),
        attendance: punches(5),
        ..DeviceState::default()
    })
    .await;
    let since = NaiveDate::from_ymd_opt(2026, 1, 1)
        .unwrap()
        .and_hms_opt(8, 3, 0)
        .unwrap();
    let got = pull_attendance_since::run(dev.config(), since).await.unwrap();
    assert_eq!(got.len(), 2);
    assert!(got.iter().all(|r| r.record.timestamp >= since));
}

#[tokio::test]
async fn live_with_shutdown_example() {
    let dev = MockDevice::start(DeviceState::default()).await;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let shutdown = async {
        stop_rx.await.expect("the test keeps the shutdown sender");
    };
    let run = live_with_shutdown::run(dev.config(), shutdown, |e| {
        tx.send(e.clone()).unwrap();
    });
    let drive = async {
        wait_subscribed(&dev).await;
        dev.push_events(2);
        let first = rx.recv().await.unwrap();
        let second = rx.recv().await.unwrap();
        stop_tx.send(()).unwrap();
        (first, second)
    };
    let (result, (first, second)) = tokio::join!(run, drive);
    result.unwrap();
    assert!(matches!(first.event, LiveEvent::Attendance(_)));
    assert_eq!((first.source, second.source), (Source::Live, Source::Live));
    assert_eq!(dev.state().event_masks, [1, 0]);
}

#[tokio::test]
async fn multi_device_live_example() {
    let a = MockDevice::start(DeviceState::default()).await;
    let b = MockDevice::start(DeviceState::default()).await;
    let devices = vec![("a".to_owned(), a.config()), ("b".to_owned(), b.config())];
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(String, zkteco_client::CapturedEvent)>(16);
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let shutdown = async {
        stop_rx.await.expect("the test keeps the shutdown sender");
    };
    let drive = async {
        wait_subscribed(&a).await;
        wait_subscribed(&b).await;
        a.push_events(1);
        b.push_events(2);
        let mut got = Vec::new();
        for _ in 0..3 {
            got.push(rx.recv().await.unwrap().0);
        }
        stop_tx.send(()).unwrap();
        got.sort();
        got
    };
    let ((), got) = tokio::join!(multi_device_live::run(devices, shutdown, tx), drive);
    assert_eq!(got, ["a", "b", "b"]);
    assert_eq!(a.state().event_masks, [1, 0]);
    assert_eq!(b.state().event_masks, [1, 0]);
}

/// A demo device for the tests: fast punches, a free port.
fn demo_options(drop_every: Option<Duration>) -> demo_device::Options {
    demo_device::Options {
        port: 0,
        interval: Duration::from_millis(60),
        drop_every,
        drop_for: Duration::from_millis(300),
        ..demo_device::Options::default()
    }
}

/// Subscribe to `demo`, with fast reconnects, and return the first event
/// that `wanted` accepts, within 10 seconds.
async fn demo_event(demo: demo_device::Demo, wanted: impl Fn(&CapturedEvent) -> bool) -> CapturedEvent {
    let opts = CaptureOptions::default()
        .reconnect(
            ReconnectPolicy::default()
                .max_attempts(u32::MAX)
                .initial_backoff(Duration::from_millis(50))
                .max_backoff(Duration::from_millis(200)),
        )
        .backfill(true);
    let mut capture = LiveCapture::connect(demo.config(), opts).await.unwrap();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let stop = async {
        stop_rx.await.expect("the test keeps the stop sender");
    };
    let watch = async {
        let found = tokio::time::timeout(Duration::from_secs(10), async {
            // Bounded by the timeout.
            loop {
                match capture.next().await {
                    Some(Ok(e)) if wanted(&e) => return e,
                    Some(_) => {}
                    None => panic!("the stream ended"),
                }
            }
        })
        .await
        .expect("an event in time");
        stop_tx.send(()).unwrap();
        found
    };
    let ((), found) = tokio::join!(demo.run(stop, |_| {}), watch);
    found
}

#[tokio::test]
async fn demo_device_example() {
    let demo = demo_device::start(demo_options(None)).await.unwrap();
    let event = demo_event(demo, |_| true).await;
    assert_eq!(event.source, Source::Live);
    let LiveEvent::Attendance(a) = event.event else {
        panic!("{event:?}");
    };
    let user: u32 = a.user_id.unwrap().as_str().parse().unwrap();
    assert!((1001..=1010).contains(&user), "{user}");
}

/// Punches recorded while the demo device is offline come back by backfill.
#[tokio::test]
async fn demo_device_drops_and_backfills() {
    let demo = demo_device::start(demo_options(Some(Duration::from_millis(500))))
        .await
        .unwrap();
    let event = demo_event(demo, |e| e.source == Source::Backfill).await;
    assert!(matches!(event.event, LiveEvent::Attendance(_)));
}
