//! Runs every example's `run` function against the mock device, so the
//! examples stay correct. Each example's `main` reads real settings and is
//! not called here.

mod support;

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
use zkteco_client::{LiveEvent, Source};

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
