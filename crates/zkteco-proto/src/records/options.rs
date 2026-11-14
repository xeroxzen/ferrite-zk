//! Configuration options (`CMD_OPTIONS_RRQ` / `CMD_OPTIONS_WRQ`).
//!
//! A read request carries `key\0`; the reply carries `key=value\0`.

use crate::error::{ProtoError, Result};

/// Option keys used for pyzk parity. A leading `~` marks read-only system
/// values on most firmware.
pub mod keys {
    /// Serial number.
    pub const SERIAL_NUMBER: &str = "~SerialNumber";
    /// Platform name.
    pub const PLATFORM: &str = "~Platform";
    /// MAC address.
    pub const MAC: &str = "MAC";
    /// Device name.
    pub const DEVICE_NAME: &str = "~DeviceName";
    /// Face algorithm version.
    pub const FACE_VERSION: &str = "ZKFaceVersion";
    /// Fingerprint algorithm version.
    pub const FP_VERSION: &str = "~ZKFPVersion";
    /// Extended attendance format flag.
    pub const EXTEND_FMT: &str = "~ExtendFmt";
    /// Extended user format flag.
    pub const USER_EXT_FMT: &str = "~UserExtFmt";
    /// Face function enabled.
    pub const FACE_FUN_ON: &str = "FaceFunOn";
    /// Old-firmware compatibility flag.
    pub const COMPAT_OLD_FIRMWARE: &str = "CompatOldFirmware";
    /// IPv4 address.
    pub const IP_ADDRESS: &str = "IPAddress";
    /// IPv4 netmask.
    pub const NET_MASK: &str = "NetMask";
    /// IPv4 gateway.
    pub const GATEWAY: &str = "GATEIPAddress";
    /// SDK build flag.
    pub const SDK_BUILD: &str = "SDKBuild";
}

fn check_key(key: &str) -> Result<()> {
    if key.is_empty() || key.contains(['\0', '=']) {
        return Err(ProtoError::InvalidField {
            field: "key",
            reason: format!("{key:?} is empty or contains NUL or '='"),
        });
    }
    Ok(())
}

/// Encode a read request: the key followed by NUL.
///
/// # Errors
///
/// [`ProtoError::InvalidField`] if the key is empty or contains NUL or `=`.
pub fn encode_read(key: &str) -> Result<Vec<u8>> {
    check_key(key)?;
    let mut out = key.as_bytes().to_vec();
    out.push(0);
    Ok(out)
}

/// Encode a write request: `key=value`, without a terminating NUL.
///
/// # Errors
///
/// [`ProtoError::InvalidField`] if the key is invalid or the value contains NUL.
#[doc = "Unverified against hardware."]
pub fn encode_write(key: &str, value: &str) -> Result<Vec<u8>> {
    check_key(key)?;
    if value.contains('\0') {
        return Err(ProtoError::InvalidField {
            field: "value",
            reason: "contains NUL".into(),
        });
    }
    Ok(format!("{key}={value}").into_bytes())
}

/// Extract the value from a read reply: the text after the first `=` and
/// before the first NUL (the whole text if there is no `=`). Invalid UTF-8
/// becomes U+FFFD.
#[must_use]
pub fn decode_value(payload: &[u8]) -> String {
    let end = payload.iter().position(|&b| b == 0).unwrap_or(payload.len());
    let text = &payload[..end];
    let start = text.iter().position(|&b| b == b'=').map_or(0, |i| i + 1);
    String::from_utf8_lossy(&text[start..]).into_owned()
}

/// Encode a read reply (`key=value\0`), as a device would send it.
#[must_use]
pub fn encode_reply(key: &str, value: &str) -> Vec<u8> {
    let mut out = format!("{key}={value}").into_bytes();
    out.push(0);
    out
}

/// Parse a numeric option value.
///
/// # Errors
///
/// [`ProtoError::InvalidField`] if the value is not an integer.
pub fn parse_int(value: &str) -> Result<i64> {
    value.trim().parse().map_err(|_| ProtoError::InvalidField {
        field: "value",
        reason: format!("{value:?} is not an integer"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_and_reply() {
        assert_eq!(encode_read(keys::SERIAL_NUMBER).unwrap(), b"~SerialNumber\0");
        assert!(encode_read("a=b").is_err());
        assert_eq!(encode_write(keys::SDK_BUILD, "1").unwrap(), b"SDKBuild=1");
        assert_eq!(decode_value(b"~SerialNumber=ABC123\0junk"), "ABC123");
        assert_eq!(decode_value(b"MAC=00:17:61:01:02:03\0"), "00:17:61:01:02:03");
        assert_eq!(decode_value(b"K=a=b\0"), "a=b");
        assert_eq!(decode_value(b"novalue\0"), "novalue");
        assert_eq!(decode_value(&encode_reply("X", "1")), "1");
        assert_eq!(parse_int(" 10 ").unwrap(), 10);
        assert!(parse_int("").is_err());
    }
}
