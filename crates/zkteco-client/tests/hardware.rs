//! Tests against a real device. Run with:
//!
//! ```sh
//! ZK_HOST=192.168.1.201 [ZK_PORT=4370] [ZK_PASSWORD=0] [ZK_TRANSPORT=udp] \
//!     cargo test -p zkteco-client --features hardware --test hardware
//! ```
//!
//! Read-only tests always run. Tests that change device data (create and
//! delete a temporary user, copy and delete a template, set the clock) run
//! only with `ZK_ALLOW_DESTRUCTIVE=1`, and clean up after themselves.
//!
//! Output never includes names, user IDs, card numbers, serial numbers or
//! MAC addresses.

#![cfg(feature = "hardware")]

use std::time::Duration;

use tokio::sync::Mutex;
use zkteco_client::{Client, ClientConfig, Error};
use zkteco_proto::records::user::{self, Privilege, User};

/// Devices usually accept one connection at a time.
static DEVICE: Mutex<()> = Mutex::const_new(());

const TEST_NAME: &str = "ferrite-test";

fn config() -> ClientConfig {
    let host = std::env::var("ZK_HOST").expect("set ZK_HOST to the device address");
    let mut config = ClientConfig::new(host);
    if let Ok(port) = std::env::var("ZK_PORT") {
        config = config.port(port.parse().expect("ZK_PORT is a port number"));
    }
    match std::env::var("ZK_TRANSPORT").as_deref() {
        Ok("udp") => config = config.transport(zkteco_proto::Transport::Udp),
        Ok("tcp") | Err(_) => {}
        Ok(other) => panic!("ZK_TRANSPORT must be tcp or udp, not {other:?}"),
    }
    if let Ok(pw) = std::env::var("ZK_PASSWORD") {
        config = config.password(pw.parse().expect("ZK_PASSWORD is a number"));
    }
    config.request_timeout(Duration::from_secs(15))
}

fn destructive_allowed() -> bool {
    let allowed = std::env::var("ZK_ALLOW_DESTRUCTIVE").is_ok_and(|v| v == "1");
    if !allowed {
        eprintln!("skipped: set ZK_ALLOW_DESTRUCTIVE=1 to run tests that change device data");
    }
    allowed
}

#[tokio::test]
async fn read_only_session() {
    let _device = DEVICE.lock().await;
    let mut client = Client::connect(config()).await.unwrap();
    eprintln!("firmware: {}", client.firmware_version().await.unwrap());
    eprintln!("platform: {}", client.platform().await.unwrap());
    eprintln!("time: {}", client.time().await.unwrap());
    eprintln!("pin width: {}", client.pin_width().await.unwrap());
    let sizes = client.read_sizes().await.unwrap();

    let users = client.users().await.unwrap();
    eprintln!(
        "users: {} decoded, {} rejected",
        users.records.len(),
        users.rejected.len()
    );
    assert_eq!(users.records.len() + users.rejected.len(), sizes.users() as usize);

    let att = client.attendance().await.unwrap();
    eprintln!(
        "attendance: {} decoded, {} rejected",
        att.records.len(),
        att.rejected.len()
    );

    let templates = client.templates().await.unwrap();
    eprintln!(
        "templates: {} decoded, {} rejected",
        templates.records.len(),
        templates.rejected.len()
    );
    if let Some(t) = templates.records.first() {
        let single = client.user_template(t.uid, t.fid).await.unwrap();
        assert_eq!(
            single.map(|s| s.data.len()),
            Some(t.data.len()),
            "single read matches table"
        );
    }
    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn wrong_password_is_unauthorized() {
    let _device = DEVICE.lock().await;
    let Some(pw) = std::env::var("ZK_PASSWORD")
        .ok()
        .and_then(|p| p.parse::<u32>().ok())
    else {
        eprintln!("skipped: needs ZK_PASSWORD (a device with a comm key)");
        return;
    };
    let err = Client::connect(config().password(pw.wrapping_add(1)))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Unauthorized), "{err:?}");
}

/// A temporary user on a free uid and user ID.
async fn temp_user(client: &mut Client) -> User {
    let users = client.users().await.unwrap().records;
    let (uid, user_id) = user::next_free_ids(&users).expect("a free uid");
    User {
        uid,
        user_id,
        name: TEST_NAME.into(),
        password: String::new(),
        privilege: Privilege::User,
        disabled: false,
        card: 0,
        group_id: String::new(),
    }
}

#[tokio::test]
async fn destructive_user_round_trip() {
    if !destructive_allowed() {
        return;
    }
    let _device = DEVICE.lock().await;
    let mut client = Client::connect(config()).await.unwrap();
    let user = temp_user(&mut client).await;
    client.set_user(&user).await.unwrap();
    let found = client
        .users()
        .await
        .unwrap()
        .records
        .into_iter()
        .find(|u| u.uid == user.uid);
    client.delete_user(user.uid).await.unwrap();
    assert_eq!(found.as_ref(), Some(&user), "user written and read back");
    let gone = client
        .users()
        .await
        .unwrap()
        .records
        .iter()
        .all(|u| u.uid != user.uid);
    assert!(gone, "user deleted");
    client.disconnect().await.unwrap();
}

/// Copies an existing template onto a temporary user, deletes it by user ID
/// (command 134, never exercised by pyzk), then removes the user.
#[tokio::test]
async fn destructive_template_delete_by_user_id() {
    if !destructive_allowed() {
        return;
    }
    let _device = DEVICE.lock().await;
    let mut client = Client::connect(config()).await.unwrap();
    let Some(source) = client.templates().await.unwrap().records.into_iter().next() else {
        eprintln!("skipped: the device has no templates to copy");
        return;
    };
    let user = temp_user(&mut client).await;
    let mut copy = source;
    copy.uid = user.uid;
    copy.fid = 0;
    let saved = client.save_user_templates(&[(user.clone(), vec![copy])]).await;
    let stored = client.user_template(user.uid, 0).await;
    let deleted = client.delete_user_template_by_user_id(&user.user_id, 0).await;
    let after = client.user_template(user.uid, 0).await;
    client.delete_user(user.uid).await.unwrap();

    saved.unwrap();
    assert!(stored.unwrap().is_some(), "template saved");
    assert!(deleted.unwrap(), "device accepted delete by user ID");
    assert!(after.unwrap().is_none(), "template gone");
    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn destructive_set_time() {
    if !destructive_allowed() {
        return;
    }
    let _device = DEVICE.lock().await;
    let mut client = Client::connect(config()).await.unwrap();
    let before = client.time().await.unwrap();
    client.set_time(before).await.unwrap();
    let after = client.time().await.unwrap();
    let drift = (after - before).num_seconds();
    assert!((0..=5).contains(&drift), "clock moved by {drift} s");
    client.disconnect().await.unwrap();
}
