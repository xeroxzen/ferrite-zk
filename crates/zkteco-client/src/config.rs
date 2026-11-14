use std::time::Duration;

use zkteco_proto::records::attendance::AttendanceFormat;
use zkteco_proto::records::user::UserFormat;
use zkteco_proto::{ChecksumPolicy, TextEncoding, Transport};

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
    /// TCP or UDP (pyzk's `force_udp`). Only TCP is implemented so far.
    pub transport: Transport,
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
}

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
            transport: Transport::Tcp,
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
        }
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
