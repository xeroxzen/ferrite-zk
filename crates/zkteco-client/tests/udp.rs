//! Client behaviour over UDP against the mock device: one packet per
//! datagram, 16 KiB chunks, loss detected by size.

mod support;

use std::time::Duration;

use support::{punches, templates, users, DeviceState, Faults, MockDevice};
use zkteco_client::{Client, Error, LiveEvent, ReplyIdPolicy, RetryPolicy, Uid};
use zkteco_proto::records::finger;
use zkteco_proto::records::user::UserFormat;
use zkteco_proto::{requests, Command, TextEncoding};

/// 72-byte users filling five 16 KiB chunks.
const MANY_USERS: u16 = 1000;

async fn device(state: DeviceState) -> MockDevice {
    MockDevice::start(state).await
}

fn big(faults: Faults) -> DeviceState {
    DeviceState {
        users: users(MANY_USERS),
        faults,
        ..DeviceState::default()
    }
}

fn read_buffer_requests(dev: &MockDevice) -> Vec<(u32, u32)> {
    dev.state()
        .log
        .iter()
        .filter(|p| p.header.command == Command::ReadBuffer)
        .map(|p| requests::decode_read_buffer(&p.payload).unwrap())
        .collect()
}

#[tokio::test]
async fn connect_auth_and_disconnect() {
    let dev = device(DeviceState {
        password: Some(4242),
        ..DeviceState::default()
    })
    .await;
    let mut client = Client::connect(dev.config_udp().password(4242)).await.unwrap();
    assert_eq!(client.session_id(), 0x2A51);
    assert_eq!(client.firmware_version().await.unwrap(), "Ver 6.60 Mock");
    client.disconnect().await.unwrap();
    assert_eq!(
        dev.commands(),
        [
            Command::Connect,
            Command::Auth,
            Command::GetVersion,
            Command::Exit
        ]
    );
    let err = Client::connect(dev.config_udp().password(1)).await.unwrap_err();
    assert!(matches!(err, Error::Unauthorized), "{err:?}");
}

#[tokio::test]
async fn unanswered_connect_times_out() {
    // Bind a socket that never answers.
    let silent = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let config = zkteco_client::ClientConfig::new("127.0.0.1")
        .transport(zkteco_proto::Transport::Udp)
        .port(silent.local_addr().unwrap().port())
        .request_timeout(Duration::from_millis(100))
        .retry(RetryPolicy::NONE);
    let err = Client::connect(config.clone()).await.unwrap_err();
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
    let err = zkteco_client::probe(&config).await.unwrap_err();
    assert!(matches!(err, Error::Timeout { .. }), "{err:?}");
}

#[tokio::test]
async fn probe_over_udp_opens_and_closes_a_session() {
    let dev = device(DeviceState::default()).await;
    zkteco_client::probe(&dev.config_udp()).await.unwrap();
    assert_eq!(dev.commands(), [Command::Connect, Command::Exit]);
}

#[tokio::test]
async fn large_tables_in_16k_chunks() {
    let dev = device(DeviceState {
        users: users(MANY_USERS),
        attendance: punches(3000),
        templates: templates(&[1, 2, 3, 4], 1400),
        ..DeviceState::default()
    })
    .await;
    let mut client = Client::connect(dev.config_udp()).await.unwrap();
    let got = client.users().await.unwrap();
    assert!(got.is_clean(), "{:?}", got.rejected);
    assert_eq!(got.records, dev.state().users);

    let total = 4 + 72 * u32::from(MANY_USERS);
    let plan: Vec<(u32, u32)> = (0..total.div_ceil(16_384))
        .map(|i| (i * 16_384, 16_384.min(total - i * 16_384)))
        .collect();
    assert_eq!(read_buffer_requests(&dev), plan);

    let att = client.attendance().await.unwrap();
    assert!(att.is_clean());
    assert_eq!(att.records, dev.state().attendance);
    let t = client.templates().await.unwrap();
    assert_eq!(t.records, dev.state().templates);
    let one = client.user_template(Uid(3), 0).await.unwrap().unwrap();
    assert_eq!(one.data.len(), 1400);
}

#[tokio::test]
async fn lost_datagram_is_a_short_read_and_retried() {
    let dev = device(big(Faults {
        lose_data_packet_once: Some(5),
        ..Faults::default()
    }))
    .await;
    let mut client = Client::connect(dev.config_udp().request_timeout(Duration::from_millis(150)))
        .await
        .unwrap();
    assert_eq!(client.users().await.unwrap().records, dev.state().users);
    let reads = read_buffer_requests(&dev);
    assert_eq!(reads[0], reads[1], "first chunk requested again");
    assert!(client.is_connected());
}

#[tokio::test]
async fn lost_datagram_without_retry_reports_short_read() {
    let dev = device(big(Faults {
        lose_data_packet_once: Some(5),
        ..Faults::default()
    }))
    .await;
    let config = dev
        .config_udp()
        .request_timeout(Duration::from_millis(150))
        .retry(RetryPolicy::NONE);
    let mut client = Client::connect(config).await.unwrap();
    let err = client.users().await.unwrap_err();
    assert!(
        matches!(
            err,
            Error::ShortRead {
                expected: 16_384,
                got: 15_360
            }
        ),
        "{err:?}"
    );
    assert!(client.is_connected(), "a lost datagram does not end the session");
    assert_eq!(dev.commands().last(), Some(&Command::FreeData));
    client.time().await.unwrap();
}

#[tokio::test]
async fn lost_datagram_mid_stream_is_retried_under_either_policy() {
    // The closing AckOk still arrives, so the shortfall is detected at once
    // and nothing late can follow.
    let dev = device(big(Faults {
        lose_data_packet_once: Some(5),
        ..Faults::default()
    }))
    .await;
    let config = dev.config_udp().reply_id_policy(ReplyIdPolicy::Lenient);
    let mut client = Client::connect(config).await.unwrap();
    assert_eq!(client.users().await.unwrap().records, dev.state().users);
}

#[tokio::test]
async fn lost_stream_tail_stalls_into_short_read_and_retries() {
    let dev = device(big(Faults {
        lose_stream_tail_once: true,
        ..Faults::default()
    }))
    .await;
    let config = dev.config_udp().request_timeout(Duration::from_millis(150));
    let mut client = Client::connect(config.clone().retry(RetryPolicy::NONE))
        .await
        .unwrap();
    let err = client.users().await.unwrap_err();
    assert!(
        matches!(
            err,
            Error::ShortRead {
                expected: 16_384,
                got: 15_360
            }
        ),
        "{err:?}"
    );
    assert!(client.is_connected());

    dev.state().faults.lose_stream_tail_once = true;
    let mut client = Client::connect(config).await.unwrap();
    assert_eq!(client.users().await.unwrap().records, dev.state().users);
}

#[tokio::test]
async fn lost_stream_tail_under_lenient_is_a_timeout() {
    let dev = device(big(Faults {
        lose_stream_tail_once: true,
        ..Faults::default()
    }))
    .await;
    let config = dev
        .config_udp()
        .request_timeout(Duration::from_millis(150))
        .reply_id_policy(ReplyIdPolicy::Lenient);
    let mut client = Client::connect(config).await.unwrap();
    let err = client.users().await.unwrap_err();
    assert!(
        matches!(
            err,
            Error::Timeout {
                command: Command::ReadBuffer,
                ..
            }
        ),
        "{err:?}"
    );
    assert!(
        !client.is_connected(),
        "without reply-ID filtering a retry is unsafe"
    );
}

#[tokio::test]
async fn duplicated_datagram_is_an_overrun_and_retried() {
    let dev = device(big(Faults {
        duplicate_data_packet_once: Some(2),
        ..Faults::default()
    }))
    .await;
    let mut client = Client::connect(dev.config_udp()).await.unwrap();
    assert_eq!(client.users().await.unwrap().records, dev.state().users);
    assert_eq!(read_buffer_requests(&dev).len(), 6, "five chunks, one retried");
}

#[tokio::test]
async fn lost_reply_to_simple_command_times_out_and_reconnects() {
    let dev = device(DeviceState {
        faults: Faults {
            drop_reply_once: Some(Command::GetTime),
            ..Faults::default()
        },
        ..DeviceState::default()
    })
    .await;
    let mut client = Client::connect(dev.config_udp().request_timeout(Duration::from_millis(100)))
        .await
        .unwrap();
    let err = client.time().await.unwrap_err();
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
    assert!(matches!(client.time().await, Err(Error::ConnectionClosed)));
    client.reconnect().await.unwrap();
    assert_eq!(client.time().await.unwrap(), dev.state().time);
}

#[tokio::test]
async fn strict_reply_ids_over_udp() {
    let dev = device(DeviceState {
        faults: Faults {
            decoy_reply_once: Some(Command::GetTime),
            ..Faults::default()
        },
        ..DeviceState::default()
    })
    .await;
    let mut client = Client::connect(dev.config_udp()).await.unwrap();
    assert_eq!(client.time().await.unwrap(), dev.state().time);
}

#[tokio::test]
async fn events_inside_udp_streams_are_kept() {
    let dev = device(big(Faults {
        events_in_streams: true,
        ..Faults::default()
    }))
    .await;
    let mut client = Client::connect(dev.config_udp()).await.unwrap();
    assert_eq!(client.users().await.unwrap().records, dev.state().users);
    let events = client.take_pending_events();
    assert_eq!(events.len(), dev.state().events_sent as usize);
    assert!(events.iter().all(|e| matches!(e, LiveEvent::Attendance(_))));
}

#[tokio::test]
async fn bulk_template_upload_over_udp() {
    let dev = device(DeviceState {
        users: users(2),
        ..DeviceState::default()
    })
    .await;
    let mut client = Client::connect(dev.config_udp()).await.unwrap();
    let user = dev.state().users[1].clone();
    let fingers = templates(&[user.uid.0], 2500);
    client
        .save_user_templates(&[(user.clone(), fingers.clone())])
        .await
        .unwrap();
    let expected = finger::encode_bulk_save(
        &[(user, fingers.clone())],
        UserFormat::Standard72,
        TextEncoding::UTF8,
    )
    .unwrap();
    assert_eq!(dev.state().saved_bulk, std::slice::from_ref(&expected));
    assert_eq!(dev.count(Command::Data), expected.len().div_ceil(1024));
    assert_eq!(
        client.user_template(Uid(2), 0).await.unwrap(),
        Some(fingers[0].clone())
    );
}

#[tokio::test]
async fn template_delete_by_user_id_resolves_uid_over_udp() {
    let dev = device(DeviceState {
        users: users(3),
        templates: templates(&[2], 50),
        ..DeviceState::default()
    })
    .await;
    let mut client = Client::connect(dev.config_udp()).await.unwrap();
    assert!(client
        .delete_user_template_by_user_id(&"1002".into(), 0)
        .await
        .unwrap());
    assert_eq!(dev.count(Command::DelUserTemp), 0, "command 134 is TCP-only");
    assert_eq!(dev.count(Command::DeleteUserTemp), 1);
    let err = client
        .delete_user_template_by_user_id(&"nope".into(), 0)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::UserNotFound(_)));
}
