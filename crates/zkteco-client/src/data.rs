//! Users, attendance and fingerprint templates.

use std::collections::HashMap;

use bytes::Bytes;

use zkteco_proto::command::table;
use zkteco_proto::records::attendance::{self, Attendance, AttendanceBatch, AttendanceFormat};
use zkteco_proto::records::finger::{self, Template};
use zkteco_proto::records::user::{self, User, UserFormat};
use zkteco_proto::{requests, Batch, Command, ProtoError, Uid, UserId};

use crate::client::Client;
use crate::error::{Error, Result};
use crate::transfer::retryable;

/// An attendance record with its user ID resolved where possible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAttendance {
    /// The record. `user_id` is filled in when it was resolved from the
    /// user table.
    pub record: Attendance,
    /// Where `record.user_id` came from.
    pub source: UserIdSource,
}

/// Where a [`ResolvedAttendance`]'s user ID came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserIdSource {
    /// The attendance record itself carries it.
    Record,
    /// Looked up by `uid` in the user table.
    UserTable,
    /// Not resolved: the record has no user ID and its `uid` is missing or
    /// matches no decoded user. `record.user_id` is `None`.
    Unresolved,
}

/// A table that can be read raw with [`Client::raw_table`]. Templates are
/// deliberately not offered: raw images bypass every decoding safeguard,
/// and template images are biometric data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawTable {
    /// The user table.
    Users,
    /// The attendance log.
    Attendance,
}

/// The byte length a table announces in its `u32` prefix.
fn announced_len(data: &[u8]) -> Result<usize> {
    let prefix: [u8; 4] = data
        .get(..4)
        .and_then(|b| b.try_into().ok())
        .ok_or(Error::Malformed(ProtoError::Truncated {
            what: "table length prefix",
            needed: 4,
            available: data.len(),
        }))?;
    Ok(usize::try_from(u32::from_le_bytes(prefix)).unwrap_or(usize::MAX))
}

fn count(n: u32) -> usize {
    usize::try_from(n).unwrap_or(usize::MAX)
}

impl Client {
    /// All users.
    ///
    /// The record layout (28, 72 or 120 bytes) comes from
    /// [`ClientConfig::user_format`](crate::ClientConfig::user_format) or is
    /// detected from the table size and the user count; it is remembered for
    /// later writes. Records that fail to decode are in
    /// [`Batch::rejected`].
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedRecordFormat`] if the layout cannot be detected;
    /// device, timeout or transfer errors.
    pub async fn users(&mut self) -> Result<Batch<User>> {
        let users = count(self.read_sizes().await?.users());
        if users == 0 {
            return Ok(Batch::default());
        }
        let data = self.raw_table(RawTable::Users).await?;
        let announced = announced_len(&data)?;
        if announced == 0 {
            tracing::warn!(users, "device reported users but sent an empty table");
            return Ok(Batch::default());
        }
        let format = match self.config.user_format {
            Some(f) => f,
            None => UserFormat::detect(announced, users)?,
        };
        self.detected_user_format = Some(format);
        Ok(user::decode_table_as(&data, format, self.config.encoding)?)
    }

    /// All attendance records.
    ///
    /// The layout (8, 16, 40 or 49 bytes) comes from
    /// [`ClientConfig::attendance_format`](crate::ClientConfig::attendance_format)
    /// or is detected. Records are returned as stored: short layouts carry
    /// only the device-internal `uid`. Use
    /// [`attendance_with_users`](Self::attendance_with_users) to resolve
    /// user IDs.
    ///
    /// # Errors
    ///
    /// As for [`users`](Self::users).
    pub async fn attendance(&mut self) -> Result<AttendanceBatch> {
        let records = count(self.read_sizes().await?.records());
        if records == 0 {
            return Ok(Batch::default());
        }
        let data = self.raw_table(RawTable::Attendance).await?;
        let announced = announced_len(&data)?;
        if announced == 0 {
            tracing::warn!(records, "device reported records but sent an empty table");
            return Ok(Batch::default());
        }
        let format = match self.config.attendance_format {
            Some(f) => f,
            None => AttendanceFormat::detect(announced, records)?,
        };
        Ok(attendance::decode_table_as(&data, format, self.config.encoding)?)
    }

    /// A table image as the device sends it: a `u32` byte length, then the
    /// records, undecoded. For diagnostics; [`users`](Self::users) and
    /// [`attendance`](Self::attendance) decode it.
    ///
    /// # Errors
    ///
    /// Device, timeout or transfer errors.
    pub async fn raw_table(&mut self, which: RawTable) -> Result<Bytes> {
        match which {
            RawTable::Users => self.read_buffered(Command::UserTempRrq, table::USERS, 0).await,
            RawTable::Attendance => self.read_buffered(Command::AttLogRrq, 0, 0).await,
        }
    }

    /// All fingerprint templates.
    ///
    /// # Errors
    ///
    /// Device, timeout or transfer errors.
    pub async fn templates(&mut self) -> Result<Batch<Template>> {
        if self.read_sizes().await?.fingers() == 0 {
            return Ok(Batch::default());
        }
        let data = self
            .read_buffered(Command::DbRrq, table::FINGER_TEMPLATES, 0)
            .await?;
        Ok(finger::decode_table(&data)?)
    }

    /// All attendance records, with user IDs resolved from the user table
    /// for records that carry only a `uid` (8-byte layout).
    ///
    /// The user table is read at most once, and only if some record needs
    /// it. Records that cannot be resolved are kept and marked
    /// [`UserIdSource::Unresolved`]; rejected records are passed through.
    ///
    /// # Errors
    ///
    /// As for [`attendance`](Self::attendance) and [`users`](Self::users).
    pub async fn attendance_with_users(&mut self) -> Result<Batch<ResolvedAttendance>> {
        let batch = self.attendance().await?;
        let ids: HashMap<Uid, UserId> = if batch.records.iter().any(|a| a.user_id.is_none()) {
            let users = self.users().await?.records;
            users.into_iter().map(|u| (u.uid, u.user_id)).collect()
        } else {
            HashMap::new()
        };
        let records = batch
            .records
            .into_iter()
            .map(|mut record| {
                let source = if record.user_id.is_some() {
                    UserIdSource::Record
                } else if let Some(id) = record.uid.and_then(|uid| ids.get(&uid)) {
                    record.user_id = Some(id.clone());
                    UserIdSource::UserTable
                } else {
                    tracing::debug!(uid = ?record.uid, "attendance record with unresolved user");
                    UserIdSource::Unresolved
                };
                ResolvedAttendance { record, source }
            })
            .collect();
        Ok(Batch {
            records,
            rejected: batch.rejected,
        })
    }

    /// One template by user index and finger (0 to 9).
    ///
    /// Returns `Ok(None)` when the device answers `CMD_ACK_ERROR`, which is
    /// how it reports a missing template. The protocol has no separate "not
    /// found" code, so `AckError` for any other reason also reads as `None`
    /// (logged at debug; DECISIONS.md D23). Every other failure reply is an
    /// error. Malformed replies are retried per the retry policy.
    ///
    /// # Errors
    ///
    /// [`Error::DeviceError`] for failure replies other than `AckError`;
    /// timeout, connection, or repeated decoding errors.
    pub async fn user_template(&mut self, uid: Uid, fid: u8) -> Result<Option<Template>> {
        let attempts = self.config.retry.attempts();
        let mut attempt = 1;
        loop {
            let reply_id = self
                .send(Command::GetUserTemp, &requests::template_by_uid(uid, fid))
                .await?;
            match self
                .receive_data(Command::GetUserTemp, reply_id, None, false)
                .await
            {
                Ok(data) => {
                    let data = finger::clean_single(&data)?;
                    return Ok(Some(Template {
                        uid,
                        fid,
                        valid: 1,
                        data,
                    }));
                }
                Err(Error::DeviceError {
                    reply: Command::AckError,
                    ..
                }) => {
                    tracing::debug!(%uid, fid, "AckError for a single template, read as \"no template\"");
                    return Ok(None);
                }
                Err(e) if retryable(&e) && !matches!(e, Error::DeviceError { .. }) && attempt < attempts => {
                    tracing::warn!(%uid, fid, attempt, %e, "template read failed; retrying");
                    self.drain().await?;
                    tokio::time::sleep(self.config.retry.backoff).await;
                    attempt += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// One template by user ID and finger, resolving the `uid` from the
    /// user table.
    ///
    /// # Errors
    ///
    /// [`Error::UserNotFound`] if no user has this ID; otherwise as for
    /// [`user_template`](Self::user_template).
    pub async fn user_template_by_user_id(&mut self, user_id: &UserId, fid: u8) -> Result<Option<Template>> {
        let uid = self.resolve_uid(user_id).await?;
        self.user_template(uid, fid).await
    }

    /// The `uid` of the user with `user_id`, from the user table.
    pub(crate) async fn resolve_uid(&mut self, user_id: &UserId) -> Result<Uid> {
        let users = self.users().await?.records;
        let mut matches = users.iter().filter(|u| &u.user_id == user_id);
        let first = matches
            .next()
            .ok_or_else(|| Error::UserNotFound(user_id.clone()))?;
        if matches.next().is_some() {
            tracing::warn!(%user_id, uid = %first.uid, "several users share this user ID; using the first");
        }
        Ok(first.uid)
    }
}
