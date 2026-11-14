//! Fixed-width, NUL-padded text fields.

use encoding_rs::Encoding;

use crate::error::{ProtoError, Result};

/// The character encoding used for names, passwords and string IDs.
///
/// Devices store text in whatever encoding they were configured with. UTF-8
/// is the default; deployments in some regions use legacy code pages such as
/// GBK or Windows-1252.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextEncoding(&'static Encoding);

impl TextEncoding {
    /// UTF-8.
    pub const UTF8: Self = Self(encoding_rs::UTF_8);

    /// Look up an encoding by WHATWG label, such as `"gbk"` or `"latin1"`.
    #[must_use]
    pub fn for_label(label: &str) -> Option<Self> {
        Encoding::for_label(label.as_bytes()).map(Self)
    }

    /// Wrap an `encoding_rs` encoding.
    #[must_use]
    pub const fn new(encoding: &'static Encoding) -> Self {
        Self(encoding)
    }

    /// The encoding's canonical name.
    #[must_use]
    pub fn name(self) -> &'static str {
        self.0.name()
    }

    /// Decode a fixed-width field: the bytes up to the first NUL.
    ///
    /// Invalid sequences become U+FFFD rather than being dropped, so a
    /// mis-configured encoding is visible in the output.
    #[must_use]
    pub fn decode_field(self, field: &[u8]) -> String {
        let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
        self.0.decode_without_bom_handling(&field[..end]).0.into_owned()
    }

    /// Encode `value` without padding.
    ///
    /// # Errors
    ///
    /// [`ProtoError::Unencodable`] if it contains characters the encoding
    /// cannot represent, or [`ProtoError::InvalidField`] if it contains NUL.
    pub fn encode(self, field: &'static str, value: &str) -> Result<Vec<u8>> {
        if value.contains('\0') {
            return Err(ProtoError::InvalidField {
                field,
                reason: "contains NUL".into(),
            });
        }
        let (bytes, _, had_errors) = self.0.encode(value);
        if had_errors {
            return Err(ProtoError::Unencodable {
                field,
                encoding: self.name(),
            });
        }
        Ok(bytes.into_owned())
    }

    /// Encode `value` into a NUL-padded field of exactly `width` bytes.
    ///
    /// A value that fills the whole field is written without a terminating
    /// NUL; the decoder accepts that.
    ///
    /// # Errors
    ///
    /// As [`TextEncoding::encode`], plus [`ProtoError::FieldTooLong`] if the
    /// encoded value exceeds `width`.
    pub fn encode_field(
        self,
        field: &'static str,
        value: &str,
        width: usize,
        out: &mut Vec<u8>,
    ) -> Result<()> {
        let bytes = self.encode(field, value)?;
        if bytes.len() > width {
            return Err(ProtoError::FieldTooLong {
                field,
                len: bytes.len(),
                max: width,
            });
        }
        out.extend_from_slice(&bytes);
        out.resize(out.len() + width - bytes.len(), 0);
        Ok(())
    }
}

impl Default for TextEncoding {
    fn default() -> Self {
        Self::UTF8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_stops_at_nul() {
        assert_eq!(TextEncoding::UTF8.decode_field(b"abc\0def"), "abc");
        assert_eq!(TextEncoding::UTF8.decode_field(b"full"), "full");
        assert_eq!(TextEncoding::UTF8.decode_field(b"\xffa\0"), "\u{fffd}a");
    }

    #[test]
    fn encode_pads_and_checks() {
        let mut out = Vec::new();
        TextEncoding::UTF8.encode_field("n", "ab", 4, &mut out).unwrap();
        assert_eq!(out, b"ab\0\0");
        out.clear();
        TextEncoding::UTF8.encode_field("n", "abcd", 4, &mut out).unwrap();
        assert_eq!(out, b"abcd");
        assert!(matches!(
            TextEncoding::UTF8.encode_field("n", "abcde", 4, &mut out),
            Err(ProtoError::FieldTooLong { len: 5, max: 4, .. })
        ));
        assert!(TextEncoding::UTF8.encode_field("n", "a\0", 4, &mut out).is_err());
    }

    #[test]
    fn legacy_encoding_rejects_unmappable() {
        let latin = TextEncoding::for_label("latin1").unwrap();
        let mut out = Vec::new();
        assert!(matches!(
            latin.encode_field("n", "\u{4e2d}", 8, &mut out),
            Err(ProtoError::Unencodable { .. })
        ));
    }
}
