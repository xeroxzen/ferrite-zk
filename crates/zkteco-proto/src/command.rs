//! Command and reply codes carried in the first header field, event flags,
//! and data-table selectors.

macro_rules! commands {
    ($( $(#[$meta:meta])* $name:ident = $value:expr, )*) => {
        /// A command or reply code (header field 0).
        ///
        /// Requests and replies share one 16-bit namespace, so both live in
        /// this enum. Codes this crate does not know are kept as
        /// [`Command::Unknown`] so that nothing is lost.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Command {
            $( $(#[$meta])* $name, )*
            /// A code not listed in the protocol manual or in this crate.
            Unknown(u16),
        }

        impl Command {
            /// Every named code, in declaration order.
            pub const ALL: &'static [Command] = &[$(Command::$name,)*];

            /// Map a wire value to a command.
            #[must_use]
            pub const fn from_u16(value: u16) -> Self {
                match value {
                    $( $value => Command::$name, )*
                    other => Command::Unknown(other),
                }
            }

            /// The wire value.
            #[must_use]
            pub const fn to_u16(self) -> u16 {
                match self {
                    $( Command::$name => $value, )*
                    Command::Unknown(v) => v,
                }
            }
        }
    };
}

commands! {
    /// Read a data table (with a [`table`] selector).
    DbRrq = 7,
    /// Upload one user record.
    UserWrq = 8,
    /// Read user or template data.
    UserTempRrq = 9,
    /// Upload a fingerprint template.
    UserTempWrq = 10,
    /// Read a configuration option (`key\0`).
    OptionsRrq = 11,
    /// Write a configuration option (`key=value`).
    OptionsWrq = 12,
    /// Read the attendance log.
    AttLogRrq = 13,
    /// Clear users, attendance and templates.
    ClearData = 14,
    /// Clear the attendance log.
    ClearAttLog = 15,
    /// Delete one user.
    DeleteUser = 18,
    /// Delete one fingerprint template (by uid and finger index).
    DeleteUserTemp = 19,
    /// Remove administrator privileges.
    ClearAdmin = 20,
    /// Read user groups.
    UserGrpRrq = 21,
    /// Write user groups.
    UserGrpWrq = 22,
    /// Read a user's time zones.
    UserTzRrq = 23,
    /// Write a user's time zones.
    UserTzWrq = 24,
    /// Read a group's time zones.
    GrpTzRrq = 25,
    /// Write a group's time zones.
    GrpTzWrq = 26,
    /// Read a time zone definition.
    TzRrq = 27,
    /// Write a time zone definition.
    TzWrq = 28,
    /// Read unlock combinations.
    UlgRrq = 29,
    /// Write unlock combinations.
    UlgWrq = 30,
    /// Open the door for a duration.
    Unlock = 31,
    /// Restore access-control settings to defaults.
    ClearAcc = 32,
    /// Clear the operation log.
    ClearOpLog = 33,
    /// Read the operation log.
    OpLogRrq = 34,
    /// Read record counts and capacities.
    GetFreeSizes = 50,
    /// Return the device to normal working state.
    EnableClock = 57,
    /// Enter verification mode.
    StartVerify = 60,
    /// Start enrolling a fingerprint.
    StartEnroll = 61,
    /// Cancel an enrolment or capture in progress.
    CancelCapture = 62,
    /// Read device state.
    StateRrq = 64,
    /// Write text to the LCD.
    WriteLcd = 66,
    /// Clear the LCD.
    ClearLcd = 67,
    /// Read the PIN (user ID) width.
    GetPinWidth = 69,
    /// Upload a short message.
    SmsWrq = 70,
    /// Download a short message.
    SmsRrq = 71,
    /// Delete a short message.
    DeleteSms = 72,
    /// Assign a short message to a user.
    UDataWrq = 73,
    /// Remove a user's short message.
    DeleteUData = 74,
    /// Read the door state.
    DoorStateRrq = 75,
    /// Write a Mifare card.
    WriteMifare = 76,
    /// Clear a Mifare card.
    EmptyMifare = 78,
    /// Read one fingerprint template by uid and finger index. Not in the manual.
    GetUserTemp = 88,
    /// Save users and templates in bulk from an uploaded buffer. Not in the manual.
    SaveUserTemps = 110,
    /// Delete one template by user ID string and finger index. Not in the manual.
    DelUserTemp = 134,
    /// Read the device clock.
    GetTime = 201,
    /// Set the device clock.
    SetTime = 202,
    /// Register for real-time events; also the command code of event packets.
    RegEvent = 500,
    /// Open a session.
    Connect = 1000,
    /// Close a session.
    Exit = 1001,
    /// Re-enable the keypad and sensors.
    EnableDevice = 1002,
    /// Lock the keypad and sensors ("working..." on the LCD).
    DisableDevice = 1003,
    /// Reboot.
    Restart = 1004,
    /// Power off.
    PowerOff = 1005,
    /// Enter idle state.
    Sleep = 1006,
    /// Leave idle state.
    Resume = 1007,
    /// Capture a fingerprint image.
    CaptureFinger = 1009,
    /// Test whether a fingerprint template exists.
    TestTemp = 1011,
    /// Capture a full image.
    CaptureImage = 1012,
    /// Apply changed data (after user or template writes).
    RefreshData = 1013,
    /// Apply changed options.
    RefreshOption = 1014,
    /// Play a built-in voice prompt.
    TestVoice = 1017,
    /// Read the firmware version string.
    GetVersion = 1100,
    /// Change the serial line speed.
    ChangeSpeed = 1101,
    /// Authenticate with the comm key.
    Auth = 1102,
    /// Announce a data transfer and its size.
    PrepareData = 1500,
    /// One packet of a data transfer.
    Data = 1501,
    /// Release the device's transfer buffer.
    FreeData = 1502,
    /// Ask the device to stage a data table for chunked reading. Not in the manual.
    PrepareBuffer = 1503,
    /// Read one chunk of a staged data table. Not in the manual.
    ReadBuffer = 1504,
    /// Reply: success.
    AckOk = 2000,
    /// Reply: the command failed.
    AckError = 2001,
    /// Reply: data follows.
    AckData = 2002,
    /// Reply: retry (the manual lists this under registered events).
    AckRetry = 2003,
    /// Reply: repeat.
    AckRepeat = 2004,
    /// Reply: the session is not authenticated.
    AckUnauth = 2005,
    /// Reply: data error.
    AckErrorData = 0xFFFB,
    /// Reply: device not initialised.
    AckErrorInit = 0xFFFC,
    /// Reply: invalid command.
    AckErrorCmd = 0xFFFD,
    /// Reply: unknown command.
    AckUnknown = 0xFFFF,
}

impl Command {
    /// Whether a reply with this code means the request succeeded.
    ///
    /// `PrepareData` and `Data` count as success because the device uses
    /// them to start or deliver a data transfer in reply to a request.
    #[must_use]
    pub const fn is_success_reply(self) -> bool {
        matches!(self, Self::AckOk | Self::PrepareData | Self::Data)
    }
}

impl From<u16> for Command {
    fn from(value: u16) -> Self {
        Self::from_u16(value)
    }
}

impl From<Command> for u16 {
    fn from(value: Command) -> Self {
        value.to_u16()
    }
}

bitflags::bitflags! {
    /// Real-time event flags.
    ///
    /// Sent in the `CMD_REG_EVENT` payload to subscribe, and carried in the
    /// `session_id` header field of each event packet the device sends.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct EventFlags: u32 {
        /// Successful verification (attendance punch).
        const ATTLOG = 1;
        /// Finger placed on the sensor.
        const FINGER = 1 << 1;
        /// User enrolled.
        const ENROLL_USER = 1 << 2;
        /// Fingerprint enrolled.
        const ENROLL_FINGER = 1 << 3;
        /// Key pressed.
        const BUTTON = 1 << 4;
        /// Door unlocked.
        const UNLOCK = 1 << 5;
        /// Fingerprint verified.
        const VERIFY = 1 << 7;
        /// Fingerprint minutiae captured.
        const FPFTR = 1 << 8;
        /// Alarm.
        const ALARM = 1 << 9;
    }
}

/// Data-table selectors used with buffered reads of [`Command::DbRrq`] and
/// [`Command::UserTempRrq`].
pub mod table {
    /// Attendance log.
    pub const ATTLOG: u32 = 1;
    /// Fingerprint templates.
    pub const FINGER_TEMPLATES: u32 = 2;
    /// Operation log.
    pub const OPLOG: u32 = 4;
    /// Users.
    pub const USERS: u32 = 5;
    /// Short messages.
    pub const SMS: u32 = 6;
    /// User short-message assignments.
    pub const USER_SMS: u32 = 7;
    /// Work codes.
    pub const WORKCODE: u32 = 8;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_named_code_round_trips_and_is_unique() {
        let mut seen = std::collections::HashSet::new();
        for &cmd in Command::ALL {
            let v = cmd.to_u16();
            assert!(seen.insert(v), "duplicate code {v}");
            assert_eq!(Command::from_u16(v), cmd);
            assert!(!matches!(cmd, Command::Unknown(_)));
        }
        assert_eq!(seen.len(), Command::ALL.len());
    }

    #[test]
    fn unknown_codes_are_preserved() {
        assert_eq!(Command::from_u16(4242), Command::Unknown(4242));
        assert_eq!(Command::Unknown(4242).to_u16(), 4242);
    }

    #[test]
    fn manual_values() {
        assert_eq!(Command::Connect.to_u16(), 1000);
        assert_eq!(Command::Auth.to_u16(), 1102);
        assert_eq!(Command::RegEvent.to_u16(), 500);
        assert_eq!(Command::AckUnauth.to_u16(), 2005);
        assert_eq!(Command::AckUnknown.to_u16(), 0xFFFF);
        assert_eq!(EventFlags::ALARM.bits(), 512);
    }
}
