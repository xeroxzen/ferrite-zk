//! Request payload encoders and small reply decoders, one per command.

use std::time::Duration;

use chrono::NaiveDateTime;

use crate::command::{Command, EventFlags};
use crate::error::{need, ProtoError, Result};
use crate::records::ids::{Uid, UserId};
use crate::records::u32_at;
use crate::text::TextEncoding;
use crate::time;

/// Payload of [`Command::GetPinWidth`].
pub const PIN_WIDTH_REQUEST: &[u8] = b" P";

/// [`Command::Unlock`]: how long to release the lock, in tenths of a second.
///
/// # Errors
///
/// [`ProtoError::InvalidField`] if the duration does not fit.
pub fn unlock(duration: Duration) -> Result<[u8; 4]> {
    let tenths = u32::try_from(duration.as_millis() / 100).map_err(|_| ProtoError::InvalidField {
        field: "duration",
        reason: "too long".into(),
    })?;
    Ok(tenths.to_le_bytes())
}

/// [`Command::TestVoice`]: index of the built-in prompt.
#[must_use]
pub fn test_voice(index: u32) -> [u8; 4] {
    index.to_le_bytes()
}

/// [`Command::WriteLcd`]: line number, a zero byte, a space, then the text.
///
/// # Errors
///
/// Fails if the text cannot be encoded.
pub fn write_lcd(line: i16, text: &str, enc: TextEncoding) -> Result<Vec<u8>> {
    let mut out = line.to_le_bytes().to_vec();
    out.extend_from_slice(&[0, b' ']);
    out.extend(enc.encode("text", text)?);
    Ok(out)
}

/// [`Command::SetTime`]: packed timestamp.
///
/// # Errors
///
/// [`ProtoError::YearOutOfRange`] outside 2000..=2099.
pub fn set_time(t: NaiveDateTime) -> Result<[u8; 4]> {
    Ok(time::encode_packed(t)?.to_le_bytes())
}

/// Decode the reply to [`Command::GetTime`].
///
/// # Errors
///
/// Fails if the payload is short or the timestamp invalid.
pub fn decode_time(payload: &[u8]) -> Result<NaiveDateTime> {
    need("time reply", payload, 4)?;
    time::decode_packed(u32_at(payload, 0))
}

/// [`Command::RegEvent`]: event mask (empty to unsubscribe).
#[must_use]
pub fn reg_event(flags: EventFlags) -> [u8; 4] {
    flags.bits().to_le_bytes()
}

/// [`Command::DeleteUser`]: uid.
#[must_use]
pub fn delete_user(uid: Uid) -> [u8; 2] {
    uid.0.to_le_bytes()
}

/// [`Command::DeleteUserTemp`] and [`Command::GetUserTemp`]: uid and finger index.
#[must_use]
pub fn template_by_uid(uid: Uid, fid: u8) -> [u8; 3] {
    let u = uid.0.to_le_bytes();
    [u[0], u[1], fid]
}

/// [`Command::DelUserTemp`]: user ID string (24 bytes) and finger index.
///
/// # Errors
///
/// Fails if the user ID does not fit.
#[doc = "Unverified against hardware."]
pub fn template_by_user_id(user_id: &UserId, fid: u8, enc: TextEncoding) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(25);
    enc.encode_field("user_id", user_id.as_str(), 24, &mut out)?;
    out.push(fid);
    Ok(out)
}

/// [`Command::StartEnroll`] over TCP: user ID string (24 bytes), finger
/// index, and a flag byte of 1.
///
/// # Errors
///
/// Fails if the user ID does not fit.
#[doc = "Unverified against hardware."]
pub fn start_enroll_tcp(user_id: &UserId, fid: u8, enc: TextEncoding) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(26);
    enc.encode_field("user_id", user_id.as_str(), 24, &mut out)?;
    out.extend_from_slice(&[fid, 1]);
    Ok(out)
}

/// [`Command::StartEnroll`] over UDP: numeric user ID (`u32`) and finger
/// index.
///
/// # Errors
///
/// [`ProtoError::InvalidField`] if the user
/// ID is not a `u32`.
#[doc = "Unverified against hardware."]
pub fn start_enroll_udp(user_id: &UserId, fid: u8) -> Result<[u8; 5]> {
    let n: u32 = user_id
        .as_str()
        .parse()
        .map_err(|_| crate::ProtoError::InvalidField {
            field: "user_id",
            reason: format!("{user_id:?} is not a u32"),
        })?;
    let u = n.to_le_bytes();
    Ok([u[0], u[1], u[2], u[3], fid])
}

/// [`Command::PrepareBuffer`]: stage a data table for chunked reading.
/// Layout: `0x01`, `u16` command, `u32` table selector, `u32` extra.
#[must_use]
pub fn prepare_buffer(command: Command, table: u32, ext: u32) -> [u8; 11] {
    let mut out = [0u8; 11];
    out[0] = 1;
    out[1..3].copy_from_slice(&command.to_u16().to_le_bytes());
    out[3..7].copy_from_slice(&table.to_le_bytes());
    out[7..11].copy_from_slice(&ext.to_le_bytes());
    out
}

/// Decode a [`Command::PrepareBuffer`] request (for device emulation).
///
/// # Errors
///
/// [`ProtoError::Truncated`] if shorter than 11 bytes.
pub fn decode_prepare_buffer(payload: &[u8]) -> Result<(Command, u32, u32)> {
    need("prepare buffer request", payload, 11)?;
    Ok((
        Command::from_u16(u16::from_le_bytes([payload[1], payload[2]])),
        u32_at(payload, 3),
        u32_at(payload, 7),
    ))
}

/// Decode the `AckOk` reply to [`Command::PrepareBuffer`]: the staged size,
/// a `u32` at offset 1. The remaining bytes have no known meaning.
///
/// # Errors
///
/// [`ProtoError::Truncated`] if shorter than 5 bytes.
pub fn decode_prepare_buffer_ack(payload: &[u8]) -> Result<u32> {
    need("prepare buffer reply", payload, 5)?;
    Ok(u32_at(payload, 1))
}

/// Encode the `AckOk` reply to [`Command::PrepareBuffer`] (for device
/// emulation): `0x00`, then the size twice, then four zero bytes.
#[must_use]
pub fn encode_prepare_buffer_ack(size: u32) -> [u8; 13] {
    let mut out = [0u8; 13];
    out[1..5].copy_from_slice(&size.to_le_bytes());
    out[5..9].copy_from_slice(&size.to_le_bytes());
    out
}

/// [`Command::ReadBuffer`]: offset and length of one chunk.
#[must_use]
pub fn read_buffer(start: u32, size: u32) -> [u8; 8] {
    let mut out = [0u8; 8];
    out[..4].copy_from_slice(&start.to_le_bytes());
    out[4..].copy_from_slice(&size.to_le_bytes());
    out
}

/// Decode a [`Command::ReadBuffer`] request (for device emulation).
///
/// # Errors
///
/// [`ProtoError::Truncated`] if shorter than 8 bytes.
pub fn decode_read_buffer(payload: &[u8]) -> Result<(u32, u32)> {
    need("read buffer request", payload, 8)?;
    Ok((u32_at(payload, 0), u32_at(payload, 4)))
}

/// [`Command::PrepareData`] sent by the host before an upload: total size.
#[must_use]
pub fn prepare_data(size: u32) -> [u8; 4] {
    size.to_le_bytes()
}

/// Decode a NUL-terminated string reply such as [`Command::GetVersion`].
#[must_use]
pub fn decode_cstring(payload: &[u8]) -> String {
    let end = payload.iter().position(|&b| b == 0).unwrap_or(payload.len());
    String::from_utf8_lossy(&payload[..end]).into_owned()
}

/// Decode the reply to [`Command::GetPinWidth`]: the first byte.
///
/// # Errors
///
/// [`ProtoError::Truncated`] if empty.
pub fn decode_pin_width(payload: &[u8]) -> Result<u8> {
    need("pin width reply", payload, 1)?;
    Ok(payload[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_payloads() {
        assert_eq!(unlock(Duration::from_secs(3)).unwrap(), 30u32.to_le_bytes());
        assert_eq!(
            write_lcd(1, "Hi", TextEncoding::UTF8).unwrap(),
            [1, 0, 0, b' ', b'H', b'i']
        );
        assert_eq!(write_lcd(0, "", TextEncoding::UTF8).unwrap(), [0, 0, 0, b' ']);
        assert_eq!(template_by_uid(Uid(0x1234), 6), [0x34, 0x12, 6]);
        assert_eq!(
            prepare_buffer(Command::DbRrq, 0x0403_0201, 0x0807_0605),
            [1, 7, 0, 1, 2, 3, 4, 5, 6, 7, 8]
        );
        let (c, t, e) = decode_prepare_buffer(&prepare_buffer(Command::AttLogRrq, 0, 0)).unwrap();
        assert_eq!((c, t, e), (Command::AttLogRrq, 0, 0));
        assert_eq!(
            decode_prepare_buffer_ack(&encode_prepare_buffer_ack(4096)).unwrap(),
            4096
        );
        assert_eq!(
            decode_read_buffer(&read_buffer(65472, 100)).unwrap(),
            (65472, 100)
        );
        assert_eq!(
            start_enroll_tcp(&"12".into(), 3, TextEncoding::UTF8)
                .unwrap()
                .len(),
            26
        );
        assert_eq!(decode_cstring(b"Ver 6.60\0x"), "Ver 6.60");
    }
}
