//! Derivation of the 4-byte `CMD_AUTH` payload from the numeric device
//! password ("comm key") and the session ID.

/// Tick value used by the reference implementations. Devices accept any value
/// because it is carried in the output.
pub const DEFAULT_TICKS: u8 = 50;

/// Derive the `CMD_AUTH` payload.
///
/// Steps: reverse the bit order of `password` (32 bits); add `session_id`
/// with 32-bit wrap-around; take the little-endian bytes and XOR them with
/// `"ZKSO"`; swap the two 16-bit halves; XOR bytes 0, 1 and 3 with `ticks` and
/// replace byte 2 with `ticks`.
#[must_use]
pub fn derive(password: u32, session_id: u16, ticks: u8) -> [u8; 4] {
    let k = password.reverse_bits().wrapping_add(u32::from(session_id));
    let b = k.to_le_bytes();
    let x = [b[0] ^ b'Z', b[1] ^ b'K', b[2] ^ b'S', b[3] ^ b'O'];
    let swapped = [x[2], x[3], x[0], x[1]];
    [swapped[0] ^ ticks, swapped[1] ^ ticks, ticks, swapped[3] ^ ticks]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_password_zero_session() {
        // k = 0 -> "ZKSO" -> swap "SOZK" -> XOR 50 except byte 2.
        assert_eq!(derive(0, 0, 50), [b'S' ^ 0x32, b'O' ^ 0x32, 0x32, b'K' ^ 0x32]);
    }

    #[test]
    fn wraps_instead_of_overflowing() {
        // reverse_bits(u32::MAX) = u32::MAX; adding a session wraps.
        let _ = derive(u32::MAX, 0xFFFF, DEFAULT_TICKS);
    }
}
