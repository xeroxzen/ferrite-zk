//! Fingerprint templates.

use bytes::Bytes;

use super::ids::Uid;
use super::user::{write_record, User, UserFormat};
use super::{split_table, u16_at, Batch};
use crate::error::{ProtoError, Result};
use crate::text::TextEncoding;

/// Size of the per-template header in template tables.
const TABLE_ENTRY_HEADER: usize = 6;

/// Highest finger index.
pub const MAX_FINGER_INDEX: u8 = 9;

/// A stored fingerprint template.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Template {
    /// Owner's device-internal index.
    pub uid: Uid,
    /// Finger index, 0 to 9.
    pub fid: u8,
    /// Validity flag as stored (1 for a normal valid template on common
    /// firmware).
    pub valid: u8,
    /// Opaque template data.
    pub data: Bytes,
}

impl Template {
    /// A valid template (flag 1) for finger `fid` of user `uid`.
    #[must_use]
    pub const fn new(uid: Uid, fid: u8, data: Bytes) -> Self {
        Self {
            uid,
            fid,
            valid: 1,
            data,
        }
    }
}

/// Decode a template table returned by a buffered read: a `u32` byte length
/// followed by variable-length entries (`u16` entry size including the
/// 6-byte header, `u16` uid, `u8` finger index, `u8` valid flag, data).
///
/// # Errors
///
/// Fails only if the length prefix is missing. Malformed entries go to
/// [`Batch::rejected`]; an entry with an impossible size ends decoding,
/// since later entries cannot be located.
#[doc = "Unverified against hardware."]
pub fn decode_table(data: &[u8]) -> Result<Batch<Template>> {
    let (announced, area, trailing) = split_table(data)?;
    let mut batch = Batch::default();
    let mut offset = 0;
    while offset < area.len() {
        let rest = &area[offset..];
        let declared = if rest.len() >= 2 {
            usize::from(u16_at(rest, 0))
        } else {
            0
        };
        if rest.len() < TABLE_ENTRY_HEADER || declared < TABLE_ENTRY_HEADER || declared > rest.len() {
            let reason = ProtoError::Truncated {
                what: "template entry",
                needed: declared.max(TABLE_ENTRY_HEADER),
                available: rest.len(),
            };
            batch.reject(offset, rest, reason);
            break;
        }
        batch.records.push(Template {
            uid: Uid(u16_at(rest, 2)),
            fid: rest[4],
            valid: rest[5],
            data: Bytes::copy_from_slice(&rest[TABLE_ENTRY_HEADER..declared]),
        });
        offset += declared;
    }
    if area.len() < announced {
        let reason = ProtoError::ShortRead {
            expected: announced,
            got: area.len(),
        };
        batch.reject(area.len(), &[], reason);
    }
    if !trailing.is_empty() {
        let reason = ProtoError::Overrun {
            expected: announced,
            got: announced + trailing.len(),
        };
        batch.reject(area.len(), trailing, reason);
    }
    Ok(batch)
}

/// Encode a template table (the inverse of [`decode_table`]).
///
/// # Errors
///
/// Fails if a template is too large for the `u16` entry size.
pub fn encode_table(templates: &[Template]) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    for t in templates {
        let size = u16::try_from(t.data.len() + TABLE_ENTRY_HEADER).map_err(|_| too_large())?;
        body.extend_from_slice(&size.to_le_bytes());
        body.extend_from_slice(&t.uid.0.to_le_bytes());
        body.push(t.fid);
        body.push(t.valid);
        body.extend_from_slice(&t.data);
    }
    let mut out = u32::try_from(body.len())
        .map_err(|_| too_large())?
        .to_le_bytes()
        .to_vec();
    out.extend(body);
    Ok(out)
}

fn too_large() -> ProtoError {
    ProtoError::InvalidField {
        field: "template",
        reason: "too large".into(),
    }
}

/// Clean up the data returned for a single-template read
/// ([`Command::GetUserTemp`](crate::Command::GetUserTemp)).
///
/// The device appends one extra byte, which is removed. If the result then
/// ends in six zero bytes, those are removed too: pyzk does this, treating
/// them as padding. A real template that ends in six zero bytes would lose
/// them.
///
/// # Errors
///
/// [`ProtoError::Truncated`] if `data` is empty.
#[doc = "Unverified against hardware."]
pub fn clean_single(data: &Bytes) -> Result<Bytes> {
    crate::error::need("single template", data, 1)?;
    let mut end = data.len() - 1;
    if end >= 6 && data[end - 6..end].iter().all(|&b| b == 0) {
        end -= 6;
    }
    Ok(data.slice(..end))
}

/// Encode the buffer uploaded before
/// [`Command::SaveUserTemps`](crate::Command::SaveUserTemps), which saves
/// users and their templates in one operation.
///
/// Layout: three `u32` section lengths (users, index, templates), then the
/// user section (each record prefixed by `0x02`), the index (per template:
/// `0x02`, `u16` uid, `0x10 + fid`, `u32` offset into the template section),
/// and the template section (per template: `u16` length, data).
///
/// # Errors
///
/// Fails if a user cannot be encoded in `format`, a finger index is above 9,
/// or a section exceeds the field sizes.
#[doc = "Unverified against hardware."]
pub fn encode_bulk_save(
    entries: &[(User, Vec<Template>)],
    format: UserFormat,
    enc: TextEncoding,
) -> Result<Vec<u8>> {
    let mut users = Vec::new();
    let mut index = Vec::new();
    let mut templates = Vec::new();
    for (user, fingers) in entries {
        users.push(0x02);
        users.extend(write_record(user, format, enc, 1)?);
        for t in fingers {
            if t.fid > MAX_FINGER_INDEX {
                return Err(ProtoError::InvalidField {
                    field: "fid",
                    reason: format!("{} is above {MAX_FINGER_INDEX}", t.fid),
                });
            }
            index.push(0x02);
            index.extend_from_slice(&user.uid.0.to_le_bytes());
            index.push(0x10 + t.fid);
            index.extend_from_slice(
                &u32::try_from(templates.len())
                    .map_err(|_| too_large())?
                    .to_le_bytes(),
            );
            templates.extend_from_slice(
                &u16::try_from(t.data.len())
                    .map_err(|_| too_large())?
                    .to_le_bytes(),
            );
            templates.extend_from_slice(&t.data);
        }
    }
    let mut out = Vec::with_capacity(12 + users.len() + index.len() + templates.len());
    for section in [&users, &index, &templates] {
        out.extend_from_slice(
            &u32::try_from(section.len())
                .map_err(|_| too_large())?
                .to_le_bytes(),
        );
    }
    out.extend(users);
    out.extend(index);
    out.extend(templates);
    Ok(out)
}

/// The payload of [`Command::SaveUserTemps`](crate::Command::SaveUserTemps)
/// after the bulk buffer has been uploaded. Meaning unknown; reproduced as
/// observed.
#[doc = "Unverified against hardware."]
pub const SAVE_USER_TEMPS_PAYLOAD: [u8; 8] = [12, 0, 0, 0, 0, 0, 8, 0];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::records::user::Privilege;

    fn tmpl(uid: u16, fid: u8, len: usize) -> Template {
        Template {
            uid: Uid(uid),
            fid,
            valid: 1,
            data: Bytes::from(
                (0..len)
                    .map(|i| u8::try_from(i % 251).unwrap() + 1)
                    .collect::<Vec<_>>(),
            ),
        }
    }

    #[test]
    fn table_round_trip() {
        let ts = vec![tmpl(1, 0, 10), tmpl(1, 6, 300), tmpl(9, 9, 0)];
        let b = decode_table(&encode_table(&ts).unwrap()).unwrap();
        assert!(b.is_clean());
        assert_eq!(b.records, ts);
    }

    #[test]
    fn table_zero_size_entry_terminates() {
        let mut t = encode_table(&[tmpl(1, 0, 10)]).unwrap();
        t.extend_from_slice(&[0, 0, 1, 0, 0, 1]);
        let len = u32::try_from(t.len() - 4).unwrap();
        t[..4].copy_from_slice(&len.to_le_bytes());
        let b = decode_table(&t).unwrap();
        assert_eq!(b.records.len(), 1);
        assert_eq!(b.rejected.len(), 1);
        assert_eq!(b.rejected[0].offset, 16);
    }

    #[test]
    fn single_template_cleanup() {
        let d = Bytes::from_static(&[1, 2, 3, 0xAA]);
        assert_eq!(&clean_single(&d).unwrap()[..], &[1, 2, 3]);
        let d = Bytes::from_static(&[1, 2, 0, 0, 0, 0, 0, 0, 0xAA]);
        assert_eq!(&clean_single(&d).unwrap()[..], &[1, 2]);
        assert!(clean_single(&Bytes::new()).is_err());
    }

    #[test]
    fn bulk_save_layout() {
        let user = User {
            uid: Uid(3),
            user_id: "3".into(),
            name: "x".into(),
            password: String::new(),
            privilege: Privilege::User,
            disabled: false,
            card: 0,
            group_id: String::new(),
        };
        let buf = encode_bulk_save(
            &[(user, vec![tmpl(3, 2, 4), tmpl(3, 5, 2)])],
            UserFormat::Standard72,
            TextEncoding::UTF8,
        )
        .unwrap();
        assert_eq!(&buf[0..12], &[73, 0, 0, 0, 16, 0, 0, 0, 10, 0, 0, 0]);
        assert_eq!(buf[12], 0x02);
        assert_eq!(buf[12 + 1 + 39], 1, "72-byte flag byte is 1 in bulk saves");
        let idx = &buf[12 + 73..12 + 73 + 16];
        assert_eq!(idx, &[2, 3, 0, 0x12, 0, 0, 0, 0, 2, 3, 0, 0x15, 6, 0, 0, 0]);
    }
}
