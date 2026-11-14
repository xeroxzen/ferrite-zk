//! Transport selection (`Transport::Auto`) and read-only mode, against the
//! mock device.

mod support;

use std::time::Duration;

use support::{users, DeviceState, MockDevice};
use zkteco_client::{Client, Error, RetryPolicy, Transport, Uid};
use zkteco_proto::Command;

#[tokio::test]
async fn auto_prefers_tcp() {
    let dev = MockDevice::start(DeviceState::default()).await;
    let client = Client::connect(dev.config().transport(Transport::Auto))
        .await
        .unwrap();
    assert_eq!(client.transport(), zkteco_proto::Transport::Tcp);
    assert_eq!(client.config().transport, Transport::Auto);
}

#[tokio::test]
async fn auto_falls_back_to_udp() {
    let dev = MockDevice::start_udp_only(DeviceState {
        users: users(30),
        ..DeviceState::default()
    })
    .await;
    let config = dev.config().transport(Transport::Auto).retry(RetryPolicy::NONE);
    let mut client = Client::connect(config.clone()).await.unwrap();
    assert_eq!(client.transport(), zkteco_proto::Transport::Udp);
    assert_eq!(client.users().await.unwrap().records, dev.state().users);
    zkteco_client::probe(&config).await.unwrap();
}

#[tokio::test]
async fn auto_does_not_fall_back_after_a_refusal() {
    let dev = MockDevice::start(DeviceState {
        password: Some(4242),
        ..DeviceState::default()
    })
    .await;
    let config = dev.config().transport(Transport::Auto).password(1);
    let err = Client::connect(config).await.unwrap_err();
    assert!(matches!(err, Error::Unauthorized), "{err:?}");
    assert_eq!(dev.state().connections, 1, "UDP was not tried");
}

#[tokio::test]
async fn auto_reports_the_udp_error_when_both_fail() {
    let silent = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let config = zkteco_client::ClientConfig::new("127.0.0.1")
        .port(silent.local_addr().unwrap().port())
        .connect_timeout(Duration::from_millis(200))
        .request_timeout(Duration::from_millis(100))
        .retry(RetryPolicy::NONE);
    assert_eq!(config.transport, Transport::Auto, "the default");
    let err = Client::connect(config).await.unwrap_err();
    assert!(
        matches!(
            err,
            Error::Timeout {
                command: Command::Connect,
                ..
            }
        ),
        "{err:?}"
    );
}

#[tokio::test]
async fn read_only_refuses_writes_before_sending() {
    let dev = MockDevice::start(DeviceState {
        users: users(3),
        ..DeviceState::default()
    })
    .await;
    let mut client = Client::connect(dev.config().read_only(true)).await.unwrap();
    let err = client.clear_attendance().await.unwrap_err();
    assert!(matches!(err, Error::ReadOnly(Command::ClearAttLog)), "{err:?}");
    let err = client.delete_user(Uid(1)).await.unwrap_err();
    assert!(matches!(err, Error::ReadOnly(Command::DeleteUser)), "{err:?}");
    let now = dev.state().time;
    let err = client.set_time(now).await.unwrap_err();
    assert!(matches!(err, Error::ReadOnly(Command::SetTime)), "{err:?}");
    assert!(client.is_connected(), "a refusal leaves the session usable");
    assert_eq!(client.users().await.unwrap().records.len(), 3);
    for command in dev.commands() {
        assert!(
            zkteco_client::READ_ONLY_COMMANDS.contains(&command),
            "{command:?} reached the device"
        );
    }
}
