//! Operations that change users and templates. All are destructive.

use zkteco_proto::records::finger::{self, Template};
use zkteco_proto::records::user::{self, User, UserFormat};
use zkteco_proto::{requests, Command, RecordKind, Transport, Uid, UserId};

use crate::client::{failure, Client};
use crate::compat;
use crate::error::{Error, Result};

impl Client {
    /// **Destructive.** Create or overwrite a user (`CMD_USER_WRQ`), then
    /// refresh the device's data.
    ///
    /// The layout comes from [`ClientConfig::user_format`](crate::ClientConfig::user_format),
    /// or from the user table (read now if needed).
    ///
    /// # Errors
    ///
    /// [`Error::InvalidArgument`] if a field does not fit the layout or the
    /// privilege is [`Unknown`](zkteco_proto::records::user::Privilege::Unknown);
    /// [`Error::UnsupportedRecordFormat`] if the layout cannot be
    /// determined; device or timeout errors.
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
    pub async fn delete_user(&mut self, uid: Uid) -> Result<()> {
        self.ack(Command::DeleteUser, &requests::delete_user(uid)).await?;
        self.refresh_data().await
    }

    /// **Destructive.** Delete a user by user ID, resolving the `uid` from
    /// the user table.
    ///
    /// # Errors
    ///
    /// [`Error::UserNotFound`] if no user has this ID; otherwise as for
    /// [`delete_user`](Self::delete_user).
    pub async fn delete_user_by_user_id(&mut self, user_id: &UserId) -> Result<()> {
        let uid = self.resolve_uid(user_id).await?;
        self.delete_user(uid).await
    }

    /// **Destructive.** Delete one template by user index and finger
    /// (`CMD_DELETE_USERTEMP`). Returns `false` if the device answers
    /// `AckError`, which usually means there was no such template.
    ///
    /// # Errors
    ///
    /// Other failure replies; timeout or connection errors.
    pub async fn delete_user_template(&mut self, uid: Uid, fid: u8) -> Result<bool> {
        let payload = requests::template_by_uid(uid, fid);
        self.try_delete(Command::DeleteUserTemp, &payload).await
    }

    /// **Destructive.** Delete one template by user ID and finger. Returns
    /// `false` if the device answers `AckError`.
    ///
    /// Over TCP this sends command 134 with the user ID (unverified). Over
    /// UDP, as pyzk, it resolves the `uid` from the user table and uses
    /// [`delete_user_template`](Self::delete_user_template).
    ///
    /// # Errors
    ///
    /// [`Error::InvalidArgument`] if the user ID does not fit;
    /// [`Error::UserNotFound`] over UDP if no user has the ID; other failure
    /// replies; timeout or connection errors.
    #[doc = "Unverified against hardware."]
    pub async fn delete_user_template_by_user_id(&mut self, user_id: &UserId, fid: u8) -> Result<bool> {
        if self.transport() == Transport::Udp {
            let uid = self.resolve_uid(user_id).await?;
            return self.delete_user_template(uid, fid).await;
        }
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

    /// The user layout for writes: configured, detected earlier, detected
    /// now from the user table, or looked up in the compatibility table
    /// (`docs/COMPATIBILITY.md`, D27) from the device's firmware, platform
    /// and format options.
    ///
    /// No option or firmware value is documented to identify the layout
    /// (PROTOCOL.md 9.1; D23), so only verified hardware reports can
    /// answer; otherwise this fails, reporting the device's values.
    async fn user_format_for_write(&mut self) -> Result<UserFormat> {
        if let Some(f) = self.config.user_format.or(self.detected.user) {
            return Ok(f);
        }
        self.users().await?;
        if let Some(f) = self.detected.user {
            return Ok(f);
        }
        let facts = self.device_facts().await?;
        if let Some(f) = compat::user_format_for(compat::COMPATIBILITY, &facts) {
            tracing::info!(?facts, ?f, "user layout from the compatibility table");
            return Ok(f);
        }
        Err(Error::UnsupportedRecordFormat {
            kind: RecordKind::User,
            detail: format!(
                "cannot determine the user record layout: the device has no users to measure, and no \
                 verified row in docs/COMPATIBILITY.md matches it (model {:?}, firmware {:?}, platform \
                 {:?}, ~UserExtFmt={:?}, ~ExtendFmt={:?}). Set ClientConfig::user_format",
                facts.model, facts.firmware, facts.platform, facts.user_ext_fmt, facts.extend_fmt
            ),
        })
    }
}
