//! Exit codes and how errors map to them (see [`crate::cli::EXIT_CODES_HELP`]).

use std::fmt;

/// A process exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Exit {
    /// 0.
    Ok = 0,
    /// 1: any other error.
    Error = 1,
    /// 2: bad arguments, missing confirmation, unknown device.
    Usage = 2,
    /// 3: connection error or timeout.
    Connection = 3,
    /// 4: authentication failed.
    Auth = 4,
    /// 5: the device refused a command, or a table kept changing while it
    /// was read (transient; safe to retry).
    Device = 5,
    /// 6: partial success.
    Partial = 6,
    /// 7: aborted by the user.
    Aborted = 7,
}

impl Exit {
    /// The numeric code.
    #[must_use]
    pub fn code(self) -> u8 {
        self as u8
    }

    /// The more serious of two outcomes, for commands over several devices:
    /// any failure outranks partial success, which outranks success.
    #[must_use]
    pub fn worst(self, other: Self) -> Self {
        let rank = |e: Self| match e {
            Self::Ok => 0,
            Self::Partial => 1,
            other => 2 + other as u8,
        };
        if rank(other) > rank(self) {
            other
        } else {
            self
        }
    }
}

/// An error with a chosen exit code: usage errors and aborts.
#[derive(Debug)]
pub struct Fail {
    /// The exit code.
    pub exit: Exit,
    /// What to tell the user.
    pub message: String,
}

impl Fail {
    /// A usage error (exit 2).
    pub fn usage(message: impl Into<String>) -> anyhow::Error {
        Self {
            exit: Exit::Usage,
            message: message.into(),
        }
        .into()
    }

    /// An abort (exit 7).
    pub fn aborted(message: impl Into<String>) -> anyhow::Error {
        Self {
            exit: Exit::Aborted,
            message: message.into(),
        }
        .into()
    }
}

impl fmt::Display for Fail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Fail {}

/// The exit code for an error: the first [`Fail`] or client error in its
/// chain decides.
#[must_use]
pub fn classify(err: &anyhow::Error) -> Exit {
    use zkteco_client::Error as E;
    for cause in err.chain() {
        if let Some(f) = cause.downcast_ref::<Fail>() {
            return f.exit;
        }
        if let Some(e) = cause.downcast_ref::<E>() {
            return match e {
                E::Io(_) | E::Timeout { .. } | E::ConnectionClosed => Exit::Connection,
                E::Unauthorized => Exit::Auth,
                E::DeviceError { .. } | E::ReadOnly(_) | E::TableChanging { .. } => Exit::Device,
                _ => Exit::Error,
            };
        }
    }
    Exit::Error
}

#[cfg(test)]
mod tests {
    use super::*;
    use zkteco_proto::Command;

    #[test]
    fn client_errors_map_to_codes() {
        let wrap = |e: zkteco_client::Error| anyhow::Error::from(e).context("doing something");
        assert_eq!(
            classify(&wrap(zkteco_client::Error::ConnectionClosed)),
            Exit::Connection
        );
        assert_eq!(classify(&wrap(zkteco_client::Error::Unauthorized)), Exit::Auth);
        let refused = zkteco_client::Error::DeviceError {
            command: Command::SetTime,
            reply: Command::AckError,
        };
        assert_eq!(classify(&wrap(refused)), Exit::Device);
        let changing = zkteco_client::Error::TableChanging {
            kind: zkteco_proto::RecordKind::Attendance,
            attempts: 3,
        };
        assert_eq!(classify(&wrap(changing)), Exit::Device);
        assert_eq!(classify(&Fail::usage("x")), Exit::Usage);
        assert_eq!(classify(&anyhow::anyhow!("other")), Exit::Error);
    }

    #[test]
    fn worst_prefers_failures() {
        assert_eq!(Exit::Ok.worst(Exit::Partial), Exit::Partial);
        assert_eq!(Exit::Partial.worst(Exit::Connection), Exit::Connection);
        assert_eq!(Exit::Connection.worst(Exit::Ok), Exit::Connection);
    }
}
