//! Users, attendance and fingerprint templates.

use zkteco_proto::command::table;
use zkteco_proto::records::attendance::{self, AttendanceBatch, AttendanceFormat};
use zkteco_proto::records::finger::{self, Template};
use zkteco_proto::records::user::{self, User, UserFormat};
use zkteco_proto::{requests, Batch, Command, ProtoError, RecordKind};

use crate::client::{failure, Client};
use crate::error::{Error, Result};
use crate::transfer::retryable;

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
        let data = self.read_buffered(Command::UserTempRrq, table::USERS, 0).await?;
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
    /// or is detected. Short layouts carry only the device-internal `uid`;
    /// unlike pyzk, user IDs are not filled in from the user table (join on
    /// `uid` with [`users`](Self::users) if needed).
    ///
    /// # Errors
    ///
    /// As for [`users`](Self::users).
    pub async fn attendance(&mut self) -> Result<AttendanceBatch> {
        let records = count(self.read_sizes().await?.records());
        if records == 0 {
            return Ok(Batch::default());
        }
        let data = self.read_buffered(Command::AttLogRrq, 0, 0).await?;
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

    /// One template by user index and finger (0 to 9), or `None` if the
    /// device reports an error for it (pyzk treats that as "no template").
    ///
    /// Malformed replies are retried per the retry policy.
    ///
    /// # Errors
    ///
    /// Timeout, connection, or repeated decoding errors.
    pub async fn user_template(&mut self, uid: u16, fid: u8) -> Result<Option<Template>> {
        let attempts = self.config.retry.attempts();
        let mut attempt = 1;
        loop {
            self.send(Command::GetUserTemp, &requests::template_by_uid(uid, fid))
                .await?;
            match self.receive_data(Command::GetUserTemp, None).await {
                Ok(data) => {
                    let data = finger::clean_single(&data)?;
                    return Ok(Some(Template {
                        uid,
                        fid,
                        valid: 1,
                        data,
                    }));
                }
                Err(Error::DeviceError { .. }) => return Ok(None),
                Err(e) if retryable(&e) && attempt < attempts => {
                    tracing::warn!(uid, fid, attempt, %e, "template read failed; retrying");
                    self.drain().await?;
                    tokio::time::sleep(self.config.retry.backoff).await;
                    attempt += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// **Destructive.** Create or overwrite a user (`CMD_USER_WRQ`), then
    /// refresh the device's data.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidArgument`] if a field does not fit the layout or the
    /// privilege is [`Unknown`](zkteco_proto::records::user::Privilege::Unknown);
    /// [`Error::UnsupportedRecordFormat`] if the layout is unknown (see
    /// [`ClientConfig::user_format`](crate::ClientConfig::user_format));
    /// device or timeout errors.
    pub async fn set_user(&mut self, user: &User) -> Result<()> {
        let format = self.user_format_for_write().await?;
        let record =
            user::encode_record(user, format, self.config.encoding).map_err(Error::InvalidArgument)?;
        self.ack(Command::UserWrq, &record).await?;
        self.refresh_data().await
    }

    /// **Destructive.** Delete a user by device index, then refresh.
    ///
    /// # Errors
    ///
    /// Device or timeout errors.
    pub async fn delete_user(&mut self, uid: u16) -> Result<()> {
        self.ack(Command::DeleteUser, &requests::delete_user(uid)).await?;
        self.refresh_data().await
    }

    /// **Destructive.** Delete one template by user index and finger
    /// (`CMD_DELETE_USERTEMP`). Returns `false` if the device refuses, which
    /// usually means there was no such template.
    ///
    /// # Errors
    ///
    /// Timeout or connection errors.
    pub async fn delete_user_template(&mut self, uid: u16, fid: u8) -> Result<bool> {
        let payload = requests::template_by_uid(uid, fid);
        self.try_delete(Command::DeleteUserTemp, &payload).await
    }

    /// **Destructive.** Delete one template by user ID and finger (command
    /// 134, TCP only). Returns `false` if the device refuses.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidArgument`] if the user ID does not fit; timeout or
    /// connection errors.
    #[doc = "Unverified against hardware."]
    pub async fn delete_user_template_by_user_id(&mut self, user_id: &str, fid: u8) -> Result<bool> {
        let payload = requests::template_by_user_id(user_id, fid, self.config.encoding)
            .map_err(Error::InvalidArgument)?;
        self.try_delete(Command::DelUserTemp, &payload).await
    }

    async fn try_delete(&mut self, command: Command, payload: &[u8]) -> Result<bool> {
        let reply = self.request(command, payload).await?;
        match reply.header.command {
            Command::AckOk => Ok(true),
            Command::AckError => Ok(false),
            other => Err(failure(command, other)),
        }
    }

    /// **Destructive.** Save users with their templates in one bulk upload
    /// (`CMD_SAVE_USERTEMPS`), then refresh. Existing users with the same
    /// `uid` are overwritten.
    ///
    /// # Errors
    ///
    /// As for [`set_user`](Self::set_user), plus [`Error::InvalidArgument`]
    /// for a finger index above 9.
    #[doc = "Unverified against hardware."]
    pub async fn save_user_templates(&mut self, entries: &[(User, Vec<Template>)]) -> Result<()> {
        let format = self.user_format_for_write().await?;
        let buffer = finger::encode_bulk_save(entries, format, self.config.encoding)
            .map_err(Error::InvalidArgument)?;
        self.upload(&buffer).await?;
        self.ack(Command::SaveUserTemps, &finger::SAVE_USER_TEMPS_PAYLOAD)
            .await?;
        self.refresh_data().await
    }

    /// The user layout for writes: configured, detected earlier, or detected
    /// now by reading the user table.
    async fn user_format_for_write(&mut self) -> Result<UserFormat> {
        if let Some(f) = self.config.user_format.or(self.detected_user_format) {
            return Ok(f);
        }
        self.users().await?;
        self.detected_user_format
            .ok_or_else(|| Error::UnsupportedRecordFormat {
                kind: RecordKind::User,
                detail: "the device has no users to detect the layout from; set ClientConfig::user_format"
                    .into(),
            })
    }
}
