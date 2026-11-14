//! Record counts and capacities (`CMD_GET_FREE_SIZES` reply).

use super::u32_at;
use crate::error::{need, Result};

/// Number of 32-bit words in the main block.
pub const WORDS: usize = 20;

/// Word positions within [`DeviceSizes::words`]. Positions not listed here
/// have no known meaning.
pub mod index {
    /// Enrolled users.
    pub const USERS: usize = 4;
    /// Stored fingerprint templates.
    pub const FINGERS: usize = 6;
    /// Attendance records.
    pub const RECORDS: usize = 8;
    /// Cards.
    pub const CARDS: usize = 12;
    /// Fingerprint capacity.
    pub const FINGERS_CAPACITY: usize = 14;
    /// User capacity.
    pub const USERS_CAPACITY: usize = 15;
    /// Attendance record capacity.
    pub const RECORDS_CAPACITY: usize = 16;
    /// Remaining fingerprint slots.
    pub const FINGERS_AVAILABLE: usize = 17;
    /// Remaining user slots.
    pub const USERS_AVAILABLE: usize = 18;
    /// Remaining attendance record slots.
    pub const RECORDS_AVAILABLE: usize = 19;
}

/// Decoded `CMD_GET_FREE_SIZES` reply: 20 little-endian 32-bit words,
/// optionally followed by 3 words of face information.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeviceSizes {
    /// The main block, raw.
    pub words: [u32; WORDS],
    /// The face block, raw: faces, unknown, face capacity.
    #[doc = "Unverified against hardware."]
    pub face_words: Option<[u32; 3]>,
}

impl DeviceSizes {
    /// Decode a reply payload.
    ///
    /// # Errors
    ///
    /// [`ProtoError::Truncated`](crate::ProtoError::Truncated) if shorter than 80 bytes.
    pub fn decode(payload: &[u8]) -> Result<Self> {
        need("device sizes", payload, WORDS * 4)?;
        let mut words = [0u32; WORDS];
        for (i, w) in words.iter_mut().enumerate() {
            *w = u32_at(payload, i * 4);
        }
        let face = &payload[WORDS * 4..];
        let face_words = (face.len() >= 12).then(|| [u32_at(face, 0), u32_at(face, 4), u32_at(face, 8)]);
        Ok(Self { words, face_words })
    }

    /// Encode as a reply payload.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out: Vec<u8> = self.words.iter().flat_map(|w| w.to_le_bytes()).collect();
        if let Some(f) = self.face_words {
            out.extend(f.iter().flat_map(|w| w.to_le_bytes()));
        }
        out
    }

    /// Enrolled users.
    #[must_use]
    pub fn users(&self) -> u32 {
        self.words[index::USERS]
    }

    /// Stored fingerprint templates.
    #[must_use]
    pub fn fingers(&self) -> u32 {
        self.words[index::FINGERS]
    }

    /// Attendance records.
    #[must_use]
    pub fn records(&self) -> u32 {
        self.words[index::RECORDS]
    }

    /// Cards.
    #[must_use]
    pub fn cards(&self) -> u32 {
        self.words[index::CARDS]
    }

    /// Fingerprint capacity.
    #[must_use]
    pub fn fingers_capacity(&self) -> u32 {
        self.words[index::FINGERS_CAPACITY]
    }

    /// User capacity.
    #[must_use]
    pub fn users_capacity(&self) -> u32 {
        self.words[index::USERS_CAPACITY]
    }

    /// Attendance record capacity.
    #[must_use]
    pub fn records_capacity(&self) -> u32 {
        self.words[index::RECORDS_CAPACITY]
    }

    /// Remaining fingerprint slots.
    #[must_use]
    pub fn fingers_available(&self) -> u32 {
        self.words[index::FINGERS_AVAILABLE]
    }

    /// Remaining user slots.
    #[must_use]
    pub fn users_available(&self) -> u32 {
        self.words[index::USERS_AVAILABLE]
    }

    /// Remaining attendance record slots.
    #[must_use]
    pub fn records_available(&self) -> u32 {
        self.words[index::RECORDS_AVAILABLE]
    }

    /// Stored faces, when the device reports face information.
    #[doc = "Unverified against hardware."]
    #[must_use]
    pub fn faces(&self) -> Option<u32> {
        self.face_words.map(|f| f[0])
    }

    /// Face capacity, when the device reports face information.
    #[doc = "Unverified against hardware."]
    #[must_use]
    pub fn faces_capacity(&self) -> Option<u32> {
        self.face_words.map(|f| f[2])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_short_input() {
        let mut s = DeviceSizes::default();
        s.words[index::USERS] = 7;
        s.words[index::RECORDS_CAPACITY] = 100_000;
        s.face_words = Some([3, 0, 400]);
        let d = DeviceSizes::decode(&s.encode()).unwrap();
        assert_eq!(d, s);
        assert_eq!(d.users(), 7);
        assert_eq!(d.faces_capacity(), Some(400));
        assert!(DeviceSizes::decode(&[0; 79]).is_err());
        assert_eq!(DeviceSizes::decode(&[0; 85]).unwrap().face_words, None);
    }
}
