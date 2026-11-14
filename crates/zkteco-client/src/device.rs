//! Device information, settings and control commands.

use std::time::Duration;

use chrono::NaiveDateTime;
use zkteco_proto::records::options::{self, keys};
use zkteco_proto::records::sizes::DeviceSizes;
use zkteco_proto::{requests, Command};

use crate::client::Client;
use crate::error::{Error, Result};

/// Network settings reported by the device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkParams {
    /// IP address.
    pub ip: String,
    /// Subnet mask.
    pub mask: String,
    /// Default gateway.
    pub gateway: String,
}

impl Client {
    /// Record counts and capacities (`CMD_GET_FREE_SIZES`).
    ///
    /// # Errors
    ///
    /// Device, timeout or decoding errors.
    pub async fn read_sizes(&mut self) -> Result<DeviceSizes> {
        let reply = self.ack(Command::GetFreeSizes, &[]).await?;
        Ok(DeviceSizes::decode(&reply.payload)?)
    }

    /// Firmware version string (`CMD_GET_VERSION`).
    ///
    /// # Errors
    ///
    /// Device or timeout errors.
    pub async fn firmware_version(&mut self) -> Result<String> {
        let reply = self.ack(Command::GetVersion, &[]).await?;
        Ok(requests::decode_cstring(&reply.payload))
    }

    /// Read one option by key (`CMD_OPTIONS_RRQ`). Returns the text after
    /// `=`, unchanged.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidArgument`] if the key cannot be encoded; device or
    /// timeout errors.
    pub async fn option(&mut self, key: &str) -> Result<String> {
        let request = options::encode_read(key).map_err(Error::InvalidArgument)?;
        let reply = self.ack(Command::OptionsRrq, &request).await?;
        Ok(options::decode_value(&reply.payload))
    }

    /// Write one option (`CMD_OPTIONS_WRQ`). pyzk's `set_sdk_build_1` is
    /// `set_option(keys::SDK_BUILD, "1")`.
    ///
    /// # Errors
    ///
    /// As for [`option`](Self::option).
    pub async fn set_option(&mut self, key: &str, value: &str) -> Result<()> {
        let request = options::encode_write(key, value).map_err(Error::InvalidArgument)?;
        self.ack(Command::OptionsWrq, &request).await.map(drop)
    }

    /// Serial number (`~SerialNumber`).
    ///
    /// # Errors
    ///
    /// As for [`option`](Self::option).
    pub async fn serial_number(&mut self) -> Result<String> {
        self.option(keys::SERIAL_NUMBER).await
    }

    /// Platform name (`~Platform`).
    ///
    /// # Errors
    ///
    /// As for [`option`](Self::option).
    pub async fn platform(&mut self) -> Result<String> {
        self.option(keys::PLATFORM).await
    }

    /// Device name (`~DeviceName`).
    ///
    /// # Errors
    ///
    /// As for [`option`](Self::option).
    pub async fn device_name(&mut self) -> Result<String> {
        self.option(keys::DEVICE_NAME).await
    }

    /// MAC address (`MAC`).
    ///
    /// # Errors
    ///
    /// As for [`option`](Self::option).
    pub async fn mac(&mut self) -> Result<String> {
        self.option(keys::MAC).await
    }

    /// Face algorithm version (`ZKFaceVersion`), as text.
    ///
    /// # Errors
    ///
    /// As for [`option`](Self::option).
    pub async fn face_version(&mut self) -> Result<String> {
        self.option(keys::FACE_VERSION).await
    }

    /// Fingerprint algorithm version (`~ZKFPVersion`), as text.
    ///
    /// # Errors
    ///
    /// As for [`option`](Self::option).
    pub async fn fp_version(&mut self) -> Result<String> {
        self.option(keys::FP_VERSION).await
    }

    /// `~ExtendFmt`, as text.
    ///
    /// # Errors
    ///
    /// As for [`option`](Self::option).
    pub async fn extend_fmt(&mut self) -> Result<String> {
        self.option(keys::EXTEND_FMT).await
    }

    /// `~UserExtFmt`, as text.
    ///
    /// # Errors
    ///
    /// As for [`option`](Self::option).
    pub async fn user_extend_fmt(&mut self) -> Result<String> {
        self.option(keys::USER_EXT_FMT).await
    }

    /// `FaceFunOn`, as text.
    ///
    /// # Errors
    ///
    /// As for [`option`](Self::option).
    pub async fn face_fun_on(&mut self) -> Result<String> {
        self.option(keys::FACE_FUN_ON).await
    }

    /// `CompatOldFirmware`, as text.
    ///
    /// # Errors
    ///
    /// As for [`option`](Self::option).
    pub async fn compat_old_firmware(&mut self) -> Result<String> {
        self.option(keys::COMPAT_OLD_FIRMWARE).await
    }

    /// IP address, mask and gateway.
    ///
    /// # Errors
    ///
    /// As for [`option`](Self::option).
    pub async fn network_params(&mut self) -> Result<NetworkParams> {
        Ok(NetworkParams {
            ip: self.option(keys::IP_ADDRESS).await?,
            mask: self.option(keys::NET_MASK).await?,
            gateway: self.option(keys::GATEWAY).await?,
        })
    }

    /// Width of user IDs (`CMD_GET_PINWIDTH`).
    ///
    /// # Errors
    ///
    /// Device, timeout or decoding errors.
    pub async fn pin_width(&mut self) -> Result<u8> {
        let reply = self
            .ack(Command::GetPinWidth, requests::PIN_WIDTH_REQUEST)
            .await?;
        Ok(requests::decode_pin_width(&reply.payload)?)
    }

    /// Device clock, as naive local time.
    ///
    /// # Errors
    ///
    /// Device, timeout or decoding errors.
    pub async fn time(&mut self) -> Result<NaiveDateTime> {
        let reply = self.ack(Command::GetTime, &[]).await?;
        Ok(requests::decode_time(&reply.payload)?)
    }

    /// Set the device clock (naive local time, years 2000 to 2099).
    ///
    /// # Errors
    ///
    /// [`Error::InvalidArgument`] outside the representable years; device
    /// or timeout errors.
    pub async fn set_time(&mut self, time: NaiveDateTime) -> Result<()> {
        let payload = requests::set_time(time).map_err(Error::InvalidArgument)?;
        self.ack(Command::SetTime, &payload).await.map(drop)
    }

    /// Enable the device's keypad and sensors (`CMD_ENABLEDEVICE`).
    ///
    /// # Errors
    ///
    /// Device or timeout errors.
    pub async fn enable_device(&mut self) -> Result<()> {
        self.ack(Command::EnableDevice, &[]).await?;
        self.device_disabled = false;
        Ok(())
    }

    /// Disable the device's keypad and sensors (`CMD_DISABLEDEVICE`), for
    /// example during bulk changes.
    ///
    /// # Errors
    ///
    /// Device or timeout errors.
    pub async fn disable_device(&mut self) -> Result<()> {
        self.ack(Command::DisableDevice, &[]).await?;
        self.device_disabled = true;
        Ok(())
    }

    /// Make the device reload its data after changes (`CMD_REFRESHDATA`).
    ///
    /// # Errors
    ///
    /// Device or timeout errors.
    pub async fn refresh_data(&mut self) -> Result<()> {
        self.ack(Command::RefreshData, &[]).await.map(drop)
    }

    /// Play a built-in voice prompt by index.
    ///
    /// # Errors
    ///
    /// Device or timeout errors.
    pub async fn test_voice(&mut self, index: u32) -> Result<()> {
        self.ack(Command::TestVoice, &requests::test_voice(index))
            .await
            .map(drop)
    }

    /// Release the door lock for `duration` (tenth-of-a-second precision).
    ///
    /// # Errors
    ///
    /// [`Error::InvalidArgument`] if the duration is too long; device or
    /// timeout errors.
    pub async fn unlock(&mut self, duration: Duration) -> Result<()> {
        let payload = requests::unlock(duration).map_err(Error::InvalidArgument)?;
        self.ack(Command::Unlock, &payload).await.map(drop)
    }

    /// Whether the device answers `CMD_DOORSTATE_RRQ` with success, as pyzk
    /// reports it. The reply payload's meaning is unknown.
    ///
    /// # Errors
    ///
    /// Timeout or connection errors. A failure reply is `Ok(false)`.
    #[doc = "Unverified against hardware."]
    pub async fn lock_state(&mut self) -> Result<bool> {
        let reply = self.request(Command::DoorStateRrq, &[]).await?;
        Ok(reply.header.command.is_success_reply())
    }

    /// Write `text` on LCD line `line`.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidArgument`] if the text cannot be encoded; device or
    /// timeout errors.
    pub async fn write_lcd(&mut self, line: i16, text: &str) -> Result<()> {
        let payload =
            requests::write_lcd(line, text, self.config.encoding).map_err(Error::InvalidArgument)?;
        self.ack(Command::WriteLcd, &payload).await.map(drop)
    }

    /// Clear the LCD.
    ///
    /// # Errors
    ///
    /// Device or timeout errors.
    pub async fn clear_lcd(&mut self) -> Result<()> {
        self.ack(Command::ClearLcd, &[]).await.map(drop)
    }

    /// Cancel a fingerprint capture in progress (`CMD_CANCELCAPTURE`).
    ///
    /// # Errors
    ///
    /// Device or timeout errors.
    pub async fn cancel_capture(&mut self) -> Result<()> {
        self.ack(Command::CancelCapture, &[]).await.map(drop)
    }

    /// Return the device to verification mode (`CMD_STARTVERIFY`).
    ///
    /// # Errors
    ///
    /// Device or timeout errors.
    pub async fn verify_user(&mut self) -> Result<()> {
        self.ack(Command::StartVerify, &[]).await.map(drop)
    }

    /// **Destructive.** Erase every attendance record (`CMD_CLEAR_ATTLOG`).
    ///
    /// # Errors
    ///
    /// Device or timeout errors.
    pub async fn clear_attendance(&mut self) -> Result<()> {
        self.ack(Command::ClearAttLog, &[]).await.map(drop)
    }

    /// **Destructive.** Erase all data: users, templates and attendance
    /// (`CMD_CLEAR_DATA`).
    ///
    /// # Errors
    ///
    /// Device or timeout errors.
    pub async fn clear_data(&mut self) -> Result<()> {
        self.ack(Command::ClearData, &[]).await.map(drop)
    }

    /// **Destructive.** Restart the device. The session ends, so this
    /// consumes the client.
    ///
    /// # Errors
    ///
    /// Device or timeout errors.
    pub async fn restart(mut self) -> Result<()> {
        let result = self.ack(Command::Restart, &[]).await;
        self.close().await;
        result.map(drop)
    }

    /// **Destructive.** Power the device off. The session ends, so this
    /// consumes the client.
    ///
    /// # Errors
    ///
    /// Device or timeout errors.
    pub async fn poweroff(mut self) -> Result<()> {
        let result = self.ack(Command::PowerOff, &[]).await;
        self.close().await;
        result.map(drop)
    }
}
