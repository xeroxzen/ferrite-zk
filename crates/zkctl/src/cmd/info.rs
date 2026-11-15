//! `zkctl info`.

use anyhow::Result;
use serde::Serialize;
use zkteco_client::Client;

use crate::cli::{Format, Global, InfoArgs};
use crate::diagnose::{sizes_of, Sizes, SCRUBBED};
use crate::exit::Exit;
use crate::output;
use crate::target::{self, Access};

/// What `info` prints.
#[derive(Debug, Serialize)]
struct Info {
    device: String,
    transport: String,
    firmware: String,
    platform: String,
    device_name: String,
    serial_number: String,
    mac: String,
    time: String,
    pin_width: u8,
    sizes: Sizes,
    /// Bytes per user record, if the user table could be measured.
    user_record_bytes: Option<usize>,
    /// Bytes per attendance record, if the log could be measured.
    attendance_record_bytes: Option<usize>,
}

pub(super) async fn run(g: &Global, args: &InfoArgs) -> Result<Exit> {
    super::one_shot(g)?;
    let target = target::single(g)?;
    let mut client = target::connect(&target, Access::ReadOnly).await?;
    let info = gather(&mut client, &target.name, args.include_identifiers).await?;
    if let Err(e) = client.disconnect().await {
        tracing::warn!(%e, "disconnect");
    }
    match g.output {
        Format::Json | Format::Ndjson => output::json(&info)?,
        Format::Human => output::text(&human(&info))?,
    }
    Ok(Exit::Ok)
}

async fn gather(client: &mut Client, name: &str, identifiers: bool) -> zkteco_client::Result<Info> {
    let (serial_number, mac) = if identifiers {
        (client.serial_number().await?, client.mac().await?)
    } else {
        (SCRUBBED.into(), SCRUBBED.into())
    };
    let sizes = client.read_sizes().await?;
    // The layouts are measured from the tables; empty tables cannot be.
    if sizes.users() > 0 {
        client.users().await?;
    }
    if sizes.records() > 0 {
        client.attendance().await?;
    }
    Ok(Info {
        device: name.to_owned(),
        transport: format!("{:?}", client.transport()).to_lowercase(),
        firmware: client.firmware_version().await?,
        platform: client.platform().await?,
        device_name: client.device_name().await?,
        serial_number,
        mac,
        time: output::device_time(client.time().await?),
        pin_width: client.pin_width().await?,
        sizes: sizes_of(&sizes),
        user_record_bytes: client
            .user_format()
            .map(zkteco_proto::records::user::UserFormat::record_len),
        attendance_record_bytes: client
            .attendance_format()
            .map(zkteco_proto::records::attendance::AttendanceFormat::record_len),
    })
}

fn human(i: &Info) -> String {
    let bytes =
        |b: Option<usize>| b.map_or_else(|| "unknown (empty table)".to_owned(), |n| format!("{n} bytes"));
    let s = &i.sizes;
    [
        ("Device", i.device.clone()),
        ("Transport", i.transport.clone()),
        ("Firmware", i.firmware.clone()),
        ("Platform", i.platform.clone()),
        ("Name", i.device_name.clone()),
        ("Serial", i.serial_number.clone()),
        ("MAC", i.mac.clone()),
        ("Time", i.time.clone()),
        ("PIN width", i.pin_width.to_string()),
        ("Users", format!("{} of {}", s.users, s.users_capacity)),
        ("Fingers", format!("{} of {}", s.fingers, s.fingers_capacity)),
        ("Records", format!("{} of {}", s.records, s.records_capacity)),
        ("User record", bytes(i.user_record_bytes)),
        ("Attendance record", bytes(i.attendance_record_bytes)),
    ]
    .iter()
    .map(|(k, v)| format!("{k:<18} {v}"))
    .collect::<Vec<_>>()
    .join("\n")
}
