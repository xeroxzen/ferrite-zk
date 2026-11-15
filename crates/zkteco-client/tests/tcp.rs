//! Client behaviour over TCP against the mock device.

mod support;

use std::time::Duration;

use chrono::NaiveDate;
use support::{punches, templates, users, Delivery, DeviceState, Faults, MockDevice};
use zkteco_client::{Client, Error, ReplyIdPolicy, RetryPolicy, Uid, UserId, UserIdSource};
use zkteco_proto::records::finger;
use zkteco_proto::records::user::{Privilege, User, UserFormat};
use zkteco_proto::{ChecksumPolicy, Command, ProtoError, TextEncoding};

async fn device(state: DeviceState) -> MockDevice {
    MockDevice::start(state).await
}

fn with_faults(faults: Faults) -> DeviceState {
    DeviceState {
        faults,
        ..DeviceState::default()
    }
}

// ---- lifecycle ------------------------------------------------------------

#[tokio::test]
async fn connect_and_disconnect_without_password() {
    let dev = device(DeviceState::default()).await;
    let client = Client::connect(dev.config()).await.unwrap();
    assert!(client.is_connected());
    assert_eq!(client.session_id(), 0x2A51);
    client.disconnect().await.unwrap();
    assert_eq!(dev.commands(), [Command::Connect, Command::Exit]);
}

#[tokio::test]
async fn connect_with_correct_password_authenticates() {
    let dev = device(DeviceState {
        password: Some(123_456),
        ..DeviceState::default()
    })
    .await;
    let mut client = Client::connect(dev.config().password(123_456)).await.unwrap();
    assert_eq!(dev.commands(), [Command::Connect, Command::Auth]);
    client.firmware_version().await.unwrap();
}

#[tokio::test]
async fn wrong_or_missing_password_is_unauthorized() {
    let dev = device(DeviceState {
        password: Some(123_456),
        ..DeviceState::default()
    })
    .await;
    let err = Client::connect(dev.config().password(1)).await.unwrap_err();
    assert!(matches!(err, Error::Unauthorized), "{err:?}");
    let err = Client::connect(dev.config()).await.unwrap_err();
    assert!(matches!(err, Error::Unauthorized), "{err:?}");
    assert_eq!(dev.state().connections, 2, "auth failures are not retried");
}

#[tokio::test]
async fn unauthorized_reply_after_login_is_typed() {
    let dev = device(with_faults(Faults {
        ack_unauth: Some(Command::GetTime),
        ..Faults::default()
    }))
    .await;
    let mut client = Client::connect(dev.config()).await.unwrap();
    assert!(matches!(client.time().await, Err(Error::Unauthorized)));
    assert!(client.is_connected());
}

#[tokio::test]
async fn refused_connection_is_io_error() {
    let dev = device(DeviceState::default()).await;
    let config = dev.config().retry(RetryPolicy::NONE);
    drop(dev);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let err = Client::connect(config).await.unwrap_err();
    assert!(matches!(err, Error::Io(_)), "{err:?}");
}

#[tokio::test]
async fn handshake_timeout_is_retried() {
    let dev = device(with_faults(Faults {
        drop_reply_once: Some(Command::Connect),
        ..Faults::default()
    }))
    .await;
    let client = Client::connect(dev.config().request_timeout(Duration::from_millis(100)))
        .await
        .unwrap();
    assert!(client.is_connected());
    assert_eq!(dev.state().connections, 2);
}

#[tokio::test]
async fn handshake_timeout_without_retry_is_timeout() {
    let dev = device(with_faults(Faults {
        drop_reply_once: Some(Command::Connect),
        ..Faults::default()
    }))
    .await;
    let config = dev
        .config()
        .request_timeout(Duration::from_millis(100))
        .retry(RetryPolicy::NONE);
    let err = Client::connect(config).await.unwrap_err();
    assert!(
        matches!(err, Error::Timeout { command: Command::Connect, after } if after == Duration::from_millis(100)),
        "{err:?}"
    );
}

#[tokio::test]
async fn pre_check_probes_first() {
    let dev = device(DeviceState::default()).await;
    let client = Client::connect(dev.config().pre_check(true)).await.unwrap();
    assert!(client.is_connected());
    assert_eq!(dev.state().connections, 2);
    zkteco_client::probe(&dev.config()).await.unwrap();
}

// ---- timeouts and typed errors -------------------------------------------

#[tokio::test]
async fn timeout_closes_connection_until_reconnect() {
    let dev = device(with_faults(Faults {
        drop_reply_once: Some(Command::GetTime),
        ..Faults::default()
    }))
    .await;
    let mut client = Client::connect(dev.config()).await.unwrap();
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
    assert!(err.is_fatal());
    assert!(!client.is_connected());
    assert!(matches!(
        client.firmware_version().await,
        Err(Error::ConnectionClosed)
    ));

    client.reconnect().await.unwrap();
    assert_eq!(client.time().await.unwrap(), dev.state().time);
    assert_eq!(dev.state().connections, 2);
}

#[tokio::test]
async fn ack_error_is_device_error_and_not_fatal() {
    let dev = device(with_faults(Faults {
        ack_error: Some(Command::GetVersion),
        ..Faults::default()
    }))
    .await;
    let mut client = Client::connect(dev.config()).await.unwrap();
    let err = client.firmware_version().await.unwrap_err();
    assert!(
        matches!(
            err,
            Error::DeviceError {
                command: Command::GetVersion,
                reply: Command::AckError
            }
        ),
        "{err:?}"
    );
    assert!(client.is_connected());
    client.time().await.unwrap();
}

#[tokio::test]
async fn bad_checksum_follows_policy() {
    let faults = Faults {
        bad_checksum_once: Some(Command::GetTime),
        ..Faults::default()
    };
    let dev = device(with_faults(faults.clone())).await;
    let mut client = Client::connect(dev.config().checksum_policy(ChecksumPolicy::Reject))
        .await
        .unwrap();
    let err = client.time().await.unwrap_err();
    assert!(
        matches!(err, Error::Malformed(ProtoError::ChecksumMismatch { .. })),
        "{err:?}"
    );
    assert!(
        client.is_connected(),
        "the frame was consumed; the stream is in sync"
    );
    client.time().await.unwrap();

    let dev = device(with_faults(faults)).await;
    let mut client = Client::connect(dev.config()).await.unwrap();
    assert_eq!(client.time().await.unwrap(), dev.state().time, "Warn accepts");
}

#[tokio::test]
async fn device_closing_is_connection_closed() {
    let dev = device(with_faults(Faults {
        close_on: Some(Command::GetTime),
        ..Faults::default()
    }))
    .await;
    let mut client = Client::connect(dev.config()).await.unwrap();
    let err = client.time().await.unwrap_err();
    assert!(matches!(err, Error::ConnectionClosed), "{err:?}");
    assert!(!client.is_connected());
}

#[tokio::test]
async fn stray_event_is_acknowledged_and_skipped() {
    let dev = device(with_faults(Faults {
        event_before: Some(Command::GetTime),
        ..Faults::default()
    }))
    .await;
    let mut client = Client::connect(dev.config()).await.unwrap();
    assert_eq!(client.time().await.unwrap(), dev.state().time);
    client.firmware_version().await.unwrap();
    let log = dev.state().log.clone();
    let ack = log
        .iter()
        .find(|p| p.header.command == Command::AckOk)
        .expect("event acknowledged");
    assert_eq!(ack.header.reply_id, 0);
}

// ---- chunked transfers ---------------------------------------------------

/// Enough users for two TCP chunks (72-byte records, 0xFFC0 per chunk).
const MANY_USERS: u16 = 1000;

async fn read_everything(delivery: Delivery) {
    let dev = device(DeviceState {
        users: users(MANY_USERS),
        attendance: punches(2000),
        templates: templates(&[1, 2, 3], 700),
        faults: Faults {
            delivery,
            ..Faults::default()
        },
        ..DeviceState::default()
    })
    .await;
    let mut client = Client::connect(dev.config()).await.unwrap();

    let got = client.users().await.unwrap();
    assert!(got.is_clean(), "{:?}", got.rejected);
    assert_eq!(got.records, dev.state().users);

    let got = client.attendance().await.unwrap();
    assert!(got.is_clean(), "{:?}", got.rejected);
    assert_eq!(got.records, dev.state().attendance);

    let got = client.templates().await.unwrap();
    assert!(got.is_clean());
    assert_eq!(got.records, dev.state().templates);
    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn chunked_reads_separate_frames() {
    read_everything(Delivery::Separate).await;
}

#[tokio::test]
async fn chunked_reads_coalesced_frames() {
    read_everything(Delivery::Coalesced).await;
}

#[tokio::test]
async fn chunked_reads_split_frames() {
    read_everything(Delivery::Split(997)).await;
}

#[tokio::test]
async fn byte_by_byte_delivery() {
    let dev = device(DeviceState {
        users: users(20),
        faults: Faults {
            delivery: Delivery::Split(1),
            ..Faults::default()
        },
        ..DeviceState::default()
    })
    .await;
    let mut client = Client::connect(dev.config()).await.unwrap();
    assert_eq!(client.users().await.unwrap().records, dev.state().users);
    assert_eq!(client.time().await.unwrap(), dev.state().time);
}

#[tokio::test]
async fn chunk_plan_and_free_data() {
    let dev = device(DeviceState {
        users: users(MANY_USERS),
        ..DeviceState::default()
    })
    .await;
    let mut client = Client::connect(dev.config()).await.unwrap();
    client.users().await.unwrap();
    let reads: Vec<(u32, u32)> = dev
        .state()
        .log
        .iter()
        .filter(|p| p.header.command == Command::ReadBuffer)
        .map(|p| zkteco_proto::requests::decode_read_buffer(&p.payload).unwrap())
        .collect();
    let total = 4 + 72 * u32::from(MANY_USERS);
    assert_eq!(reads, [(0, 0xFFC0), (0xFFC0, total - 0xFFC0)]);
    assert_eq!(dev.commands().last(), Some(&Command::FreeData));
}

#[tokio::test]
async fn small_table_comes_inline() {
    let dev = device(DeviceState {
        users: users(3),
        ..DeviceState::default()
    })
    .await;
    let mut client = Client::connect(dev.config()).await.unwrap();
    assert_eq!(client.users().await.unwrap().records.len(), 3);
    assert_eq!(dev.count(Command::ReadBuffer), 0);
    assert_eq!(
        dev.count(Command::FreeData),
        0,
        "nothing staged, nothing to free (as pyzk)"
    );
}

#[tokio::test]
async fn short_stream_is_retried() {
    let dev = device(DeviceState {
        users: users(MANY_USERS),
        faults: Faults {
            short_stream_once: true,
            ..Faults::default()
        },
        ..DeviceState::default()
    })
    .await;
    let mut client = Client::connect(dev.config()).await.unwrap();
    assert_eq!(client.users().await.unwrap().records, dev.state().users);
    assert_eq!(dev.count(Command::ReadBuffer), 3, "first chunk read twice");
}

#[tokio::test]
async fn short_stream_without_retry_is_short_read() {
    let dev = device(DeviceState {
        users: users(MANY_USERS),
        faults: Faults {
            short_stream_once: true,
            ..Faults::default()
        },
        ..DeviceState::default()
    })
    .await;
    let mut client = Client::connect(dev.config().retry(RetryPolicy::NONE))
        .await
        .unwrap();
    let err = client.users().await.unwrap_err();
    assert!(
        matches!(err, Error::ShortRead { expected: 0xFFC0, got } if got == 0xFFC0 - 1),
        "{err:?}"
    );
    assert!(client.is_connected());
    assert_eq!(
        dev.commands().last(),
        Some(&Command::FreeData),
        "buffer released after failure"
    );
}

#[tokio::test]
async fn bad_sequence_is_drained_and_retried() {
    let dev = device(DeviceState {
        attendance: punches(2000),
        faults: Faults {
            bad_sequence_once: true,
            ..Faults::default()
        },
        ..DeviceState::default()
    })
    .await;
    let mut client = Client::connect(dev.config()).await.unwrap();
    assert_eq!(client.attendance().await.unwrap().records, dev.state().attendance);
    assert_eq!(dev.count(Command::ReadBuffer), 3);
}

#[tokio::test]
async fn read_buffer_device_error_is_retried_then_reported() {
    let dev = device(DeviceState {
        users: users(MANY_USERS),
        faults: Faults {
            ack_error: Some(Command::ReadBuffer),
            ..Faults::default()
        },
        ..DeviceState::default()
    })
    .await;
    let mut client = Client::connect(dev.config()).await.unwrap();
    let err = client.users().await.unwrap_err();
    assert!(
        matches!(
            err,
            Error::DeviceError {
                command: Command::ReadBuffer,
                ..
            }
        ),
        "{err:?}"
    );
    assert_eq!(dev.count(Command::ReadBuffer), 3);
}

#[tokio::test]
async fn empty_device_reads_nothing() {
    let dev = device(DeviceState::default()).await;
    let mut client = Client::connect(dev.config()).await.unwrap();
    assert_eq!(client.users().await.unwrap().records, Vec::new());
    assert_eq!(client.attendance().await.unwrap().records, Vec::new());
    assert_eq!(client.templates().await.unwrap().records, Vec::new());
    assert_eq!(dev.count(Command::PrepareBuffer), 0);
}

#[tokio::test]
async fn configured_layout_overrides_detection() {
    let dev = device(DeviceState {
        users: users(4),
        user_format: UserFormat::Compact28,
        ..DeviceState::default()
    })
    .await;
    let mut client = Client::connect(dev.config().user_format(UserFormat::Compact28))
        .await
        .unwrap();
    assert_eq!(client.users().await.unwrap().records, dev.state().users);
    let mut client = Client::connect(dev.config().user_format(UserFormat::Standard72))
        .await
        .unwrap();
    let batch = client.users().await.unwrap();
    assert!(
        !batch.is_clean(),
        "the wrong layout is visible as rejections, not silence"
    );
}

// ---- templates -----------------------------------------------------------

#[tokio::test]
async fn single_template_inline_streamed_and_missing() {
    let dev = device(DeviceState {
        templates: [templates(&[1], 100), templates(&[2], 3000)].concat(),
        ..DeviceState::default()
    })
    .await;
    let mut client = Client::connect(dev.config()).await.unwrap();
    for uid in [1, 2] {
        let t = client.user_template(Uid(uid), 0).await.unwrap().unwrap();
        let expected = dev
            .state()
            .templates
            .iter()
            .find(|t| t.uid == Uid(uid))
            .cloned()
            .unwrap();
        assert_eq!(t, expected);
    }
    assert_eq!(client.user_template(Uid(9), 0).await.unwrap(), None);
    assert!(client.is_connected());
}

#[tokio::test]
async fn delete_templates() {
    let mut state = DeviceState {
        users: users(2),
        templates: templates(&[1, 2], 50),
        ..DeviceState::default()
    };
    state.templates[1].fid = 3;
    let dev = device(state).await;
    let mut client = Client::connect(dev.config()).await.unwrap();
    assert!(client.delete_user_template(Uid(1), 0).await.unwrap());
    assert!(!client.delete_user_template(Uid(1), 0).await.unwrap());
    assert!(client
        .delete_user_template_by_user_id(&"1002".into(), 3)
        .await
        .unwrap());
    assert!(!client
        .delete_user_template_by_user_id(&"nobody".into(), 3)
        .await
        .unwrap());
    assert_eq!(dev.state().templates, Vec::new());
}

#[tokio::test]
async fn save_user_templates_uploads_in_1k_packets() {
    let dev = device(DeviceState {
        users: users(1),
        ..DeviceState::default()
    })
    .await;
    let mut client = Client::connect(dev.config()).await.unwrap();
    let user = dev.state().users[0].clone();
    let fingers = templates(&[user.uid.0], 1500);
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
    let cmds = dev.commands();
    let start = cmds.iter().position(|c| *c == Command::FreeData).unwrap();
    let data_packets = expected.len().div_ceil(1024);
    let mut want = vec![Command::FreeData, Command::PrepareData];
    want.extend(std::iter::repeat_n(Command::Data, data_packets));
    want.extend([Command::SaveUserTemps, Command::RefreshData]);
    assert_eq!(cmds[start..], want[..]);
    assert_eq!(
        client.user_template(Uid(1), 0).await.unwrap(),
        Some(fingers[0].clone())
    );
}

// ---- users ---------------------------------------------------------------

fn new_user(uid: u16) -> User {
    User {
        uid: Uid(uid),
        user_id: "4242".into(),
        name: "Synthetic".into(),
        password: "12".into(),
        privilege: Privilege::Admin,
        disabled: false,
        card: 99,
        group_id: "1".into(),
    }
}

#[tokio::test]
async fn set_and_delete_user() {
    let dev = device(DeviceState {
        users: users(2),
        ..DeviceState::default()
    })
    .await;
    let mut client = Client::connect(dev.config()).await.unwrap();
    client.set_user(&new_user(50)).await.unwrap();
    assert!(dev.state().users.contains(&new_user(50)));
    assert_eq!(dev.commands().last(), Some(&Command::RefreshData));
    client.delete_user(Uid(50)).await.unwrap();
    assert_eq!(dev.state().users.len(), 2);
}

#[tokio::test]
async fn set_user_on_empty_device_needs_configured_layout() {
    let dev = device(DeviceState::default()).await;
    let mut client = Client::connect(dev.config()).await.unwrap();
    let err = client.set_user(&new_user(1)).await.unwrap_err();
    assert!(matches!(err, Error::UnsupportedRecordFormat { .. }), "{err:?}");

    let mut client = Client::connect(dev.config().user_format(UserFormat::Standard72))
        .await
        .unwrap();
    client.set_user(&new_user(1)).await.unwrap();
    assert_eq!(dev.state().users, [new_user(1)]);
}

#[tokio::test]
async fn invalid_user_is_rejected_before_sending() {
    let dev = device(DeviceState::default()).await;
    let mut client = Client::connect(dev.config().user_format(UserFormat::Standard72))
        .await
        .unwrap();
    let mut user = new_user(1);
    user.privilege = Privilege::Unknown(5);
    let err = client.set_user(&user).await.unwrap_err();
    assert!(matches!(err, Error::InvalidArgument(_)), "{err:?}");
    assert_eq!(dev.count(Command::UserWrq), 0);
}

// ---- device info and control ---------------------------------------------

#[tokio::test]
async fn device_information() {
    let dev = device(DeviceState {
        users: users(5),
        attendance: punches(7),
        ..DeviceState::default()
    })
    .await;
    let mut client = Client::connect(dev.config()).await.unwrap();
    assert_eq!(client.firmware_version().await.unwrap(), "Ver 6.60 Mock");
    assert_eq!(client.serial_number().await.unwrap(), "MOCK0000000001");
    assert_eq!(client.platform().await.unwrap(), "MOCK_TFT");
    let net = client.network_params().await.unwrap();
    assert_eq!((net.ip.as_str(), net.mask.as_str()), ("127.0.0.1", "255.0.0.0"));
    assert_eq!(client.pin_width().await.unwrap(), 9);
    let sizes = client.read_sizes().await.unwrap();
    assert_eq!((sizes.users(), sizes.records()), (5, 7));
    assert!(matches!(
        client.option("Missing").await,
        Err(Error::DeviceError {
            reply: Command::AckError,
            ..
        })
    ));
    client.set_option("SDKBuild", "1").await.unwrap();
    assert_eq!(client.option("SDKBuild").await.unwrap(), "1");
}

#[tokio::test]
async fn time_round_trip() {
    let dev = device(DeviceState::default()).await;
    let mut client = Client::connect(dev.config()).await.unwrap();
    let t = NaiveDate::from_ymd_opt(2027, 6, 30)
        .unwrap()
        .and_hms_opt(23, 59, 58)
        .unwrap();
    client.set_time(t).await.unwrap();
    assert_eq!(client.time().await.unwrap(), t);
    let too_late = NaiveDate::from_ymd_opt(2100, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap();
    assert!(matches!(
        client.set_time(too_late).await,
        Err(Error::InvalidArgument(_))
    ));
}

#[tokio::test]
async fn control_commands() {
    let dev = device(DeviceState {
        attendance: punches(3),
        ..DeviceState::default()
    })
    .await;
    let mut client = Client::connect(dev.config()).await.unwrap();
    client.disable_device().await.unwrap();
    client.enable_device().await.unwrap();
    client.test_voice(0).await.unwrap();
    client.unlock(Duration::from_secs(3)).await.unwrap();
    assert!(client.lock_state().await.unwrap());
    client.write_lcd(0, "hello").await.unwrap();
    client.clear_lcd().await.unwrap();
    client.cancel_capture().await.unwrap();
    client.verify_user().await.unwrap();
    client.clear_attendance().await.unwrap();
    assert_eq!(dev.state().attendance, Vec::new());
    client.restart().await.unwrap();
    assert_eq!(dev.commands().last(), Some(&Command::Restart));
}

// ---- reply IDs -------------------------------------------------------------

#[tokio::test]
async fn strict_discards_mismatched_reply_then_accepts_matching() {
    let dev = device(with_faults(Faults {
        decoy_reply_once: Some(Command::GetTime),
        ..Faults::default()
    }))
    .await;
    let mut client = Client::connect(dev.config()).await.unwrap();
    assert_eq!(client.time().await.unwrap(), dev.state().time);
    assert!(client.is_connected());
}

#[tokio::test]
async fn strict_with_only_mismatched_replies_times_out() {
    let dev = device(with_faults(Faults {
        wrong_reply_id: Some(Command::GetTime),
        ..Faults::default()
    }))
    .await;
    let mut client = Client::connect(dev.config()).await.unwrap();
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
}

#[tokio::test]
async fn lenient_accepts_mismatched_replies() {
    let dev = device(with_faults(Faults {
        wrong_reply_id: Some(Command::GetTime),
        decoy_reply_once: Some(Command::GetVersion),
        ..Faults::default()
    }))
    .await;
    let mut client = Client::connect(dev.config().reply_id_policy(ReplyIdPolicy::Lenient))
        .await
        .unwrap();
    assert_eq!(client.time().await.unwrap(), dev.state().time);
    let err = client.firmware_version().await.unwrap_err();
    assert!(
        matches!(
            err,
            Error::DeviceError {
                reply: Command::AckError,
                ..
            }
        ),
        "the decoy is taken"
    );
}

// ---- pending events --------------------------------------------------------

fn event_ids(events: &[zkteco_client::LiveEvent]) -> Vec<String> {
    events
        .iter()
        .map(|e| match e {
            zkteco_client::LiveEvent::Attendance(a) => a.user_id.clone().unwrap().0,
            zkteco_client::LiveEvent::Raw { .. } => panic!("unexpected raw event"),
        })
        .collect()
}

async fn streaming_events_device(faults: Faults) -> MockDevice {
    device(DeviceState {
        users: users(MANY_USERS),
        faults: Faults {
            events_in_streams: true,
            ..faults
        },
        ..DeviceState::default()
    })
    .await
}

#[tokio::test]
async fn events_during_chunked_read_are_all_kept() {
    let dev = streaming_events_device(Faults::default()).await;
    let mut client = Client::connect(dev.config()).await.unwrap();
    assert_eq!(client.users().await.unwrap().records, dev.state().users);
    let sent = dev.state().events_sent;
    assert!(sent > 60, "two chunks of ~64 data packets each");
    let events = client.take_pending_events();
    let want: Vec<String> = (1..=sent).map(|i| i.to_string()).collect();
    assert_eq!(event_ids(&events), want, "all events, oldest first");
    assert_eq!(client.dropped_events(), 0);
    assert_eq!(client.take_pending_events(), Vec::new());
    let acks = dev
        .state()
        .log
        .iter()
        .filter(|p| p.header.command == Command::AckOk)
        .count();
    assert_eq!(acks, sent as usize, "every event acknowledged");
}

#[tokio::test]
async fn events_in_a_drained_stream_are_kept() {
    let dev = streaming_events_device(Faults {
        bad_sequence_once: true,
        ..Faults::default()
    })
    .await;
    let mut client = Client::connect(dev.config()).await.unwrap();
    assert_eq!(client.users().await.unwrap().records, dev.state().users);
    let sent = dev.state().events_sent;
    assert_eq!(client.take_pending_events().len(), sent as usize);
}

#[tokio::test]
async fn full_queue_drops_oldest_and_counts() {
    let dev = streaming_events_device(Faults::default()).await;
    let mut client = Client::connect(dev.config().pending_event_capacity(3))
        .await
        .unwrap();
    client.users().await.unwrap();
    let sent = dev.state().events_sent;
    let events = client.take_pending_events();
    let want: Vec<String> = (sent - 2..=sent).map(|i| i.to_string()).collect();
    assert_eq!(event_ids(&events), want, "newest three kept");
    assert_eq!(client.dropped_events(), u64::from(sent - 3));
}

// ---- layouts, attendance resolution, identities ----------------------------

#[tokio::test]
async fn undetectable_user_format_error_reports_evidence() {
    let dev = device(DeviceState::default()).await;
    let mut client = Client::connect(dev.config()).await.unwrap();
    let Error::UnsupportedRecordFormat { detail, .. } = client.set_user(&new_user(1)).await.unwrap_err()
    else {
        panic!("expected UnsupportedRecordFormat");
    };
    assert!(detail.contains("Ver 6.60 Mock"), "{detail}");
    assert!(detail.contains(r#"~UserExtFmt="unavailable""#), "{detail}");
    assert!(detail.contains("COMPATIBILITY.md"), "{detail}");
    assert!(detail.contains("ClientConfig::user_format"), "{detail}");
    assert!(client.is_connected());
}

#[tokio::test]
async fn attendance_with_users_resolves_and_flags() {
    let dev = device(DeviceState {
        users: users(40),
        attendance: punches(100)
            .into_iter()
            .map(|mut a| {
                a.user_id = None;
                a
            })
            .collect(),
        attendance_format: zkteco_proto::records::attendance::AttendanceFormat::Short8,
        ..DeviceState::default()
    })
    .await;
    let mut client = Client::connect(dev.config()).await.unwrap();
    let batch = client.attendance_with_users().await.unwrap();
    assert_eq!(batch.records.len(), 100, "nothing dropped");
    for r in &batch.records {
        let uid = r.record.uid.unwrap();
        if uid.0 <= 40 {
            assert_eq!(r.source, UserIdSource::UserTable);
            assert_eq!(
                r.record.user_id,
                Some(UserId(format!("{}", 1000 + u32::from(uid.0))))
            );
        } else {
            assert_eq!(r.source, UserIdSource::Unresolved);
            assert_eq!(r.record.user_id, None);
        }
    }
    let raw = client.attendance().await.unwrap();
    assert!(
        raw.records.iter().all(|a| a.user_id.is_none()),
        "raw records unchanged"
    );
}

#[tokio::test]
async fn attendance_with_users_skips_user_table_when_not_needed() {
    let dev = device(DeviceState {
        users: users(5),
        attendance: punches(10),
        ..DeviceState::default()
    })
    .await;
    let mut client = Client::connect(dev.config()).await.unwrap();
    let batch = client.attendance_with_users().await.unwrap();
    assert!(batch.records.iter().all(|r| r.source == UserIdSource::Record));
    assert_eq!(
        dev.count(Command::PrepareBuffer),
        1,
        "only the attendance table is read"
    );
}

#[tokio::test]
async fn user_template_only_none_for_ack_error() {
    let dev = device(with_faults(Faults {
        ack_unauth: Some(Command::GetUserTemp),
        ..Faults::default()
    }))
    .await;
    let mut client = Client::connect(dev.config()).await.unwrap();
    assert!(matches!(
        client.user_template(Uid(1), 0).await,
        Err(Error::Unauthorized)
    ));
    let dev = device(DeviceState::default()).await;
    let mut client = Client::connect(dev.config()).await.unwrap();
    assert_eq!(client.user_template(Uid(1), 0).await.unwrap(), None);
}

#[tokio::test]
async fn by_user_id_operations_resolve_or_report_not_found() {
    let dev = device(DeviceState {
        users: users(3),
        templates: templates(&[2], 80),
        ..DeviceState::default()
    })
    .await;
    let mut client = Client::connect(dev.config()).await.unwrap();
    let t = client.user_template_by_user_id(&"1002".into(), 0).await.unwrap();
    assert_eq!(t.map(|t| t.uid), Some(Uid(2)));
    let missing = UserId::from("9999");
    let err = client.user_template_by_user_id(&missing, 0).await.unwrap_err();
    assert!(
        matches!(err, Error::UserNotFound(ref id) if *id == missing),
        "{err:?}"
    );

    client.delete_user_by_user_id(&"1003".into()).await.unwrap();
    assert!(dev.state().users.iter().all(|u| u.uid != Uid(3)));
    let err = client.delete_user_by_user_id(&missing).await.unwrap_err();
    assert!(matches!(err, Error::UserNotFound(_)));
}
