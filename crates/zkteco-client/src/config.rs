use std::time::Duration;

use zkteco_proto::records::attendance::AttendanceFormat;
use zkteco_proto::records::user::UserFormat;
use zkteco_proto::{ChecksumPolicy, Command, TextEncoding};

/// The device's default port.
pub const DEFAULT_PORT: u16 = 4370;

/// How a [`Client`](crate::Client) connects and behaves.
///
/// ```
/// use std::time::Duration;
/// use zkteco_client::ClientConfig;
///
/// let config = ClientConfig::new("192.168.1.201")
///     .password(1234)
///     .request_timeout(Duration::from_secs(10));
/// assert_eq!(config.port, 4370);
/// ```
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ClientConfig {
    /// Device host name or address.
    pub host: String,
    /// Device port (default 4370).
    pub port: u16,
    /// TCP, UDP, or TCP with a fallback to UDP (default
    /// [`Transport::Auto`]).
    pub transport: Transport,
    /// Refuse to send any command outside [`READ_ONLY_COMMANDS`] (default
    /// `false`). Such calls fail with [`Error::ReadOnly`](crate::Error::ReadOnly)
    /// before anything is sent.
    pub read_only: bool,
    /// Communication password (the device's "comm key"). `None` sends 0 if
    /// the device asks for authentication.
    pub password: Option<u32>,
    /// Limit for the TCP connect (default 5 s).
    pub connect_timeout: Duration,
    /// Limit for each reply (default 10 s). A timeout closes the connection.
    pub request_timeout: Duration,
    /// Retries for connecting and for re-reading a corrupt chunk.
    pub retry: RetryPolicy,
    /// What to do with incoming packets whose checksum is wrong (default
    /// [`ChecksumPolicy::Warn`]).
    pub checksum_policy: ChecksumPolicy,
    /// Encoding of text fields (default UTF-8).
    pub encoding: TextEncoding,
    /// Probe the device with a separate TCP connect before connecting
    /// (default `false`). This is the inverse of pyzk's `ommit_ping`; there
    /// is no ICMP ping.
    pub pre_check: bool,
    /// Use this user record layout instead of detecting it from the table.
    pub user_format: Option<UserFormat>,
    /// Use this attendance record layout instead of detecting it.
    pub attendance_format: Option<AttendanceFormat>,
    /// What to do with replies whose reply ID does not match the request
    /// (default [`ReplyIdPolicy::Strict`]).
    pub reply_id_policy: ReplyIdPolicy,
    /// Live events kept while waiting for replies (default 1024). When full,
    /// the oldest is dropped and counted.
    pub pending_event_capacity: usize,
    /// TCP keepalive idle time (default 30 seconds): after this long
    /// without traffic the operating system probes the connection, so a
    /// device that loses power is noticed even when nothing is being sent.
    /// `None` leaves keepalive off. Ignored over UDP.
    pub tcp_keepalive: Option<Duration>,
    /// With [`read_only`](Self::read_only), also allow `CMD_REG_EVENT`, so a
    /// read-only client can subscribe to live events (default `false`).
    /// Subscribing changes no data; it ends with the session.
    pub allow_event_subscription: bool,
}

/// Which transport to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Transport {
    /// Try TCP; if the device cannot be reached over TCP, use UDP. The
    /// choice is logged at `info` and available from
    /// [`Client::transport`](crate::Client::transport).
    #[default]
    Auto,
    /// TCP only.
    Tcp,
    /// UDP only (pyzk's `force_udp`).
    ///
    /// Table reads over UDP request one datagram's worth at a time, so each
    /// reply is a single `Data` packet and reordering in transit cannot
    /// corrupt it (DECISIONS.md D25). There is no option for larger chunks:
    /// with no sequence number in UDP `Data` packets, a reordered chunk
    /// would be accepted with its bytes in the wrong order.
    Udp,
}

/// Commands a [`ClientConfig::read_only`] client may send. None of them
/// changes device data, settings or state: session control, reads, the
/// staging and release of read buffers, and acknowledgements of live events.
pub const READ_ONLY_COMMANDS: &[Command] = &[
    Command::Connect,
    Command::Auth,
    Command::Exit,
    Command::GetVersion,
    Command::OptionsRrq,
    Command::GetFreeSizes,
    Command::GetTime,
    Command::GetPinWidth,
    Command::PrepareBuffer,
    Command::ReadBuffer,
    Command::FreeData,
    Command::AckOk,
];

/// How to treat a reply whose reply ID does not match the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReplyIdPolicy {
    /// Log, discard, and keep waiting until the request times out.
    #[default]
    Strict,
    /// Log and accept, for firmware that does not echo the reply ID.
    Lenient,
}

impl ClientConfig {
    /// Defaults for `host`.
    #[must_use]
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            port: DEFAULT_PORT,
            transport: Transport::Auto,
            read_only: false,
            password: None,
            connect_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(10),
            retry: RetryPolicy::default(),
            checksum_policy: ChecksumPolicy::Warn,
            encoding: TextEncoding::UTF8,
            pre_check: false,
            user_format: None,
            attendance_format: None,
            reply_id_policy: ReplyIdPolicy::Strict,
            pending_event_capacity: 1024,
            tcp_keepalive: Some(Duration::from_secs(30)),
            allow_event_subscription: false,
        }
    }

    /// Set the TCP keepalive idle time; `None` turns keepalive off.
    #[must_use]
    pub fn tcp_keepalive(mut self, idle: Option<Duration>) -> Self {
        self.tcp_keepalive = idle;
        self
    }

    /// Allow a read-only client to subscribe to live events.
    #[must_use]
    pub fn allow_event_subscription(mut self, enabled: bool) -> Self {
        self.allow_event_subscription = enabled;
        self
    }

    /// Set the port.
    #[must_use]
    pub fn port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// Set the transport.
    #[must_use]
    pub fn transport(mut self, transport: Transport) -> Self {
        self.transport = transport;
        self
    }

    /// Enable or disable read-only mode.
    #[must_use]
    pub fn read_only(mut self, enabled: bool) -> Self {
        self.read_only = enabled;
        self
    }

    /// Set the communication password.
    #[must_use]
    pub fn password(mut self, password: u32) -> Self {
        self.password = Some(password);
        self
    }

    /// Set the connect timeout.
    #[must_use]
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Set the per-reply timeout.
    #[must_use]
    pub fn request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// Set the retry policy.
    #[must_use]
    pub fn retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    /// Set the checksum policy for incoming packets.
    #[must_use]
    pub fn checksum_policy(mut self, policy: ChecksumPolicy) -> Self {
        self.checksum_policy = policy;
        self
    }

    /// Set the text encoding.
    #[must_use]
    pub fn encoding(mut self, encoding: TextEncoding) -> Self {
        self.encoding = encoding;
        self
    }

    /// Enable or disable the reachability pre-check.
    #[must_use]
    pub fn pre_check(mut self, enabled: bool) -> Self {
        self.pre_check = enabled;
        self
    }

    /// Fix the user record layout.
    #[must_use]
    pub fn user_format(mut self, format: UserFormat) -> Self {
        self.user_format = Some(format);
        self
    }

    /// Set the reply-ID policy.
    #[must_use]
    pub fn reply_id_policy(mut self, policy: ReplyIdPolicy) -> Self {
        self.reply_id_policy = policy;
        self
    }

    /// Set the pending-event queue capacity.
    #[must_use]
    pub fn pending_event_capacity(mut self, capacity: usize) -> Self {
        self.pending_event_capacity = capacity;
        self
    }

    /// Fix the attendance record layout.
    #[must_use]
    pub fn attendance_format(mut self, format: AttendanceFormat) -> Self {
        self.attendance_format = Some(format);
        self
    }
}

/// How often to retry, and how long to wait in between.
///
/// Applies to establishing a connection and to re-requesting a chunk whose
/// reply was malformed (bad sequence, short or corrupt data). It does not
/// apply after a timeout: that closes the connection (see
/// [`Error::Timeout`](crate::Error::Timeout)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Total attempts, at least 1 (default 3, as in pyzk).
    pub attempts: u32,
    /// Delay before each retry (default 200 ms).
    pub backoff: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            attempts: 3,
            backoff: Duration::from_millis(200),
        }
    }
}

impl RetryPolicy {
    /// A single attempt.
    pub const NONE: Self = Self {
        attempts: 1,
        backoff: Duration::ZERO,
    };

    pub(crate) fn attempts(self) -> u32 {
        self.attempts.max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every command that writes, clears, deletes, changes settings or
    /// device state, or starts an interactive mode. None may be allowed in
    /// read-only mode.
    const CHANGES_THE_DEVICE: &[Command] = &[
        Command::UserWrq,
        Command::UserTempWrq,
        Command::OptionsWrq,
        Command::ClearData,
        Command::ClearAttLog,
        Command::DeleteUser,
        Command::DeleteUserTemp,
        Command::ClearAdmin,
        Command::UserGrpWrq,
        Command::UserTzWrq,
        Command::GrpTzWrq,
        Command::TzWrq,
        Command::UlgWrq,
        Command::Unlock,
        Command::ClearAcc,
        Command::ClearOpLog,
        Command::EnableClock,
        Command::StartVerify,
        Command::StartEnroll,
        Command::CancelCapture,
        Command::WriteLcd,
        Command::ClearLcd,
        Command::SmsWrq,
        Command::DeleteSms,
        Command::UDataWrq,
        Command::DeleteUData,
        Command::WriteMifare,
        Command::EmptyMifare,
        Command::SaveUserTemps,
        Command::DelUserTemp,
        Command::SetTime,
        Command::RegEvent,
        Command::EnableDevice,
        Command::DisableDevice,
        Command::Restart,
        Command::PowerOff,
        Command::Sleep,
        Command::Resume,
        Command::CaptureFinger,
        Command::CaptureImage,
        Command::RefreshData,
        Command::RefreshOption,
        Command::TestVoice,
        Command::ChangeSpeed,
        Command::PrepareData,
        Command::Data,
    ];

    #[test]
    fn read_only_allowlist_is_exactly_the_reviewed_set() {
        let expected = [
            (Command::Connect, "opens the session"),
            (Command::Auth, "authenticates the session"),
            (Command::Exit, "closes the session"),
            (Command::GetVersion, "reads the firmware version"),
            (Command::OptionsRrq, "reads one option"),
            (Command::GetFreeSizes, "reads counters"),
            (Command::GetTime, "reads the clock"),
            (Command::GetPinWidth, "reads the user ID width"),
            (Command::PrepareBuffer, "stages a table for reading"),
            (Command::ReadBuffer, "reads a staged chunk"),
            (Command::FreeData, "releases the read buffer"),
            (Command::AckOk, "acknowledges a live event"),
        ];
        let allowed: Vec<_> = expected.iter().map(|(c, _)| *c).collect();
        assert_eq!(READ_ONLY_COMMANDS, allowed.as_slice());
    }

    #[test]
    fn read_only_allowlist_excludes_every_change() {
        for command in CHANGES_THE_DEVICE {
            assert!(!READ_ONLY_COMMANDS.contains(command), "{command:?} is allowed");
        }
        // Every code, named or not: only allowlisted ones pass.
        for code in 0..=u16::MAX {
            let command = Command::from(code);
            if READ_ONLY_COMMANDS.contains(&command) {
                assert!(!CHANGES_THE_DEVICE.contains(&command));
                assert!(
                    !matches!(command, Command::Unknown(_)),
                    "{code} is not a named command"
                );
            }
        }
    }
}
