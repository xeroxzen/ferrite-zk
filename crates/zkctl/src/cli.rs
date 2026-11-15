//! Command-line definition. Public so that `xtask` can generate man pages
//! and completions from it.

use std::path::PathBuf;
use std::time::Duration;

use clap::{ArgAction, ArgGroup, Args, Parser, Subcommand, ValueEnum};
use zkteco_client::Transport;

use crate::duration::parse_duration;

/// Exit codes, shown in `zkctl --help`.
pub const EXIT_CODES_HELP: &str = "\
Exit codes:
  0  success
  1  error
  2  usage error (bad arguments, missing --yes, unknown device)
  3  connection error or timeout
  4  authentication failed
  5  the device refused a command
  6  partial success (for example, rejected records; see stderr)
  7  aborted (confirmation declined, or interrupted)

Data goes to standard output; logs and warnings to standard error.";

/// Command-line tool for `ZKTeco` devices.
#[derive(Debug, Parser)]
#[command(name = "zkctl", version, about, after_help = EXIT_CODES_HELP)]
pub struct Cli {
    /// Options shared by all commands.
    #[command(flatten)]
    pub global: Global,
    /// The command.
    #[command(subcommand)]
    pub command: Command,
}

/// Device targeting, output and logging, accepted by every command.
#[derive(Debug, Clone, Args)]
pub struct Global {
    /// Named device from the config file. Repeatable for `attendance pull`
    /// and `live`.
    #[arg(short = 'd', long = "device", value_name = "NAME", global = true)]
    pub devices: Vec<String>,
    /// Device host name or address (instead of `-d`).
    #[arg(long, global = true, conflicts_with = "devices")]
    pub host: Option<String>,
    /// Device port [default: 4370, or the profile's].
    #[arg(long, global = true)]
    pub port: Option<u16>,
    /// Transport [default: auto, or the profile's].
    #[arg(long, value_enum, global = true)]
    pub transport: Option<TransportArg>,
    /// Per-reply timeout, such as `5s` or `500ms` [default: 10s, or the
    /// profile's].
    #[arg(long, value_parser = parse_duration, global = true)]
    pub timeout: Option<Duration>,
    /// Comm key (communication password). Prefer `ZK_PASSWORD` or the
    /// keyring (`zkctl device set-password`), which stay out of shell
    /// history.
    #[arg(long, env = "ZK_PASSWORD", hide_env_values = true, global = true)]
    pub password: Option<u32>,
    /// Output format. `json` for one-shot results, `ndjson` for one JSON
    /// object per line (streams and record lists). Machine formats are
    /// stable; `human` may change.
    #[arg(long, value_enum, default_value_t = Format::Human, global = true)]
    pub output: Format,
    /// Config file [default: the platform config directory, for example
    /// `~/.config/zkctl/config.toml` on Linux].
    #[arg(long, env = "ZKCTL_CONFIG", value_name = "FILE", global = true)]
    pub config: Option<PathBuf>,
    /// More logging on stderr (`-v` info, `-vv` debug, `-vvv` trace).
    #[arg(short, long, action = ArgAction::Count, global = true)]
    pub verbose: u8,
    /// Less logging on stderr (errors only).
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    pub quiet: bool,
}

/// Output formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    /// Tables and text for people.
    Human,
    /// One JSON document.
    Json,
    /// One JSON object per line.
    Ndjson,
}

/// Transport choice on the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TransportArg {
    /// TCP, falling back to UDP.
    Auto,
    /// TCP only.
    Tcp,
    /// UDP only.
    Udp,
}

impl From<TransportArg> for Transport {
    fn from(t: TransportArg) -> Self {
        match t {
            TransportArg::Auto => Self::Auto,
            TransportArg::Tcp => Self::Tcp,
            TransportArg::Udp => Self::Udp,
        }
    }
}

/// Commands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Firmware, platform, name, counters and record layouts.
    Info(InfoArgs),
    /// Read or set the device clock.
    #[command(subcommand)]
    Time(TimeCommand),
    /// List, add, update or delete users.
    #[command(subcommand)]
    Users(UsersCommand),
    /// Export or clear the attendance log.
    #[command(subcommand)]
    Attendance(AttendanceCommand),
    /// Stream live events until Ctrl-C. Read-only.
    Live(LiveArgs),
    /// Destructive: enrol a fingerprint by scanning it on the device.
    Enroll(EnrollArgs),
    /// Export or import fingerprint templates (biometric data).
    #[command(subcommand)]
    Templates(TemplatesCommand),
    /// Clear all data on the device.
    #[command(subcommand)]
    Data(DataCommand),
    /// Destructive: restart the device.
    Restart(Confirm),
    /// Destructive: power the device off.
    Poweroff(Confirm),
    /// Manage named devices in the config file.
    #[command(subcommand)]
    Device(DeviceCommand),
    /// Survey a device without changing it, and print a JSON report for
    /// docs/COMPATIBILITY.md. Read-only.
    Diagnose(DiagnoseArgs),
    /// Print a shell completion script.
    Completions {
        /// The shell.
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

/// Confirmation for destructive commands.
#[derive(Debug, Clone, Args)]
pub struct Confirm {
    /// Confirm the change. Without it nothing is done.
    #[arg(long)]
    pub yes: bool,
    /// Print exactly what would be sent, and send no writes (reads still
    /// happen, to resolve users and layouts).
    #[arg(long)]
    pub dry_run: bool,
}

/// `info`.
#[derive(Debug, Clone, Args)]
pub struct InfoArgs {
    /// Include the serial number and MAC address.
    #[arg(long)]
    pub include_identifiers: bool,
}

/// `time`.
#[derive(Debug, Subcommand)]
pub enum TimeCommand {
    /// Print the device clock (device local time, no time zone).
    Get,
    /// Destructive: set the device clock.
    #[command(group(ArgGroup::new("when").required(true).args(["time", "sync"])))]
    Set {
        /// The new time: RFC 3339 (converted to this computer's time zone)
        /// or `YYYY-MM-DDTHH:MM:SS` (device local).
        time: Option<String>,
        /// Use this computer's local time.
        #[arg(long)]
        sync: bool,
        /// Confirmation.
        #[command(flatten)]
        confirm: Confirm,
    },
}

/// Which user: by device slot or by user ID, never guessed.
#[derive(Debug, Clone, Args)]
#[group(required = true, multiple = false)]
pub struct UserRef {
    /// Device slot (internal index).
    #[arg(long)]
    pub uid: Option<u16>,
    /// The user ID people see and type.
    #[arg(long)]
    pub user_id: Option<String>,
}

/// User fields to write.
#[derive(Debug, Clone, Args)]
pub struct UserFields {
    /// Name.
    #[arg(long)]
    pub name: Option<String>,
    /// Role.
    #[arg(long, value_enum)]
    pub privilege: Option<PrivilegeArg>,
    /// PIN for password verification.
    #[arg(long)]
    pub user_password: Option<String>,
    /// Card number.
    #[arg(long)]
    pub card: Option<u32>,
    /// Group.
    #[arg(long)]
    pub group: Option<String>,
    /// Disable (`true`) or enable (`false`) the user.
    #[arg(long)]
    pub disabled: Option<bool>,
}

/// Writable roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PrivilegeArg {
    /// Ordinary user.
    User,
    /// May enrol users.
    Enroller,
    /// Manager.
    Manager,
    /// Administrator.
    Admin,
}

/// `users`.
#[derive(Debug, Subcommand)]
pub enum UsersCommand {
    /// List users.
    List {
        /// Only users whose user ID equals this, or whose name contains it
        /// (case-insensitive).
        #[arg(long)]
        filter: Option<String>,
    },
    /// Destructive: add a user.
    Add {
        /// The user ID people see and type.
        #[arg(long)]
        user_id: String,
        /// Device slot [default: the next free one].
        #[arg(long)]
        uid: Option<u16>,
        /// Fields.
        #[command(flatten)]
        fields: UserFields,
        /// Confirmation.
        #[command(flatten)]
        confirm: Confirm,
    },
    /// Destructive: change fields of an existing user.
    Update {
        /// The user.
        #[command(flatten)]
        user: UserRef,
        /// Fields to change; others keep their values.
        #[command(flatten)]
        fields: UserFields,
        /// Confirmation.
        #[command(flatten)]
        confirm: Confirm,
    },
    /// Destructive: delete a user.
    Delete {
        /// The user.
        #[command(flatten)]
        user: UserRef,
        /// Confirmation.
        #[command(flatten)]
        confirm: Confirm,
    },
}

/// `attendance`.
#[derive(Debug, Subcommand)]
pub enum AttendanceCommand {
    /// Export attendance records. Repeat `-d` to pull from several devices.
    Pull(PullArgs),
    /// Destructive and irreversible: erase the attendance log. Run
    /// `attendance pull` first.
    Clear(ClearArgs),
}

/// `attendance pull`.
#[derive(Debug, Clone, Args)]
pub struct PullArgs {
    /// Only records at or after this time: RFC 3339, `YYYY-MM-DDTHH:MM:SS`
    /// (device local), or a duration before the device's current time
    /// (`15m`, `24h`, `7d`).
    #[arg(long)]
    pub since: Option<String>,
    /// Only records before this time (same forms as `--since`).
    #[arg(long)]
    pub until: Option<String>,
    /// Fill in user IDs from the user table for records that lack them.
    #[arg(long)]
    pub with_users: bool,
    /// Cursor file: emit only records after those emitted by the previous
    /// run, and update it. For incremental export from cron.
    #[arg(long, value_name = "FILE")]
    pub state: Option<PathBuf>,
}

/// Irreversible clears.
#[derive(Debug, Clone, Args)]
pub struct ClearArgs {
    /// Confirmation.
    #[command(flatten)]
    pub confirm: Confirm,
    /// The device name, typed to confirm (otherwise asked for
    /// interactively).
    #[arg(long, value_name = "NAME")]
    pub confirm_device: Option<String>,
}

/// `live`.
#[derive(Debug, Clone, Args)]
pub struct LiveArgs {
    /// Fill in user IDs and uids from the user table.
    #[arg(long)]
    pub with_users: bool,
    /// Reconnect after a lost connection, indefinitely, with backoff.
    #[arg(long)]
    pub reconnect: bool,
    /// After a reconnect, deliver the punches stored while disconnected.
    #[arg(long, requires = "reconnect")]
    pub backfill: bool,
    /// Stop after this many events.
    #[arg(long)]
    pub count: Option<u64>,
    /// Stop after this long (`30s`, `10m`, `8h`).
    #[arg(long, value_parser = parse_duration)]
    pub duration: Option<Duration>,
}

/// `enroll`.
#[derive(Debug, Clone, Args)]
pub struct EnrollArgs {
    /// The user ID to enrol.
    #[arg(long)]
    pub user_id: String,
    /// Finger index, 0 to 9.
    #[arg(long, value_parser = clap::value_parser!(u8).range(0..=9))]
    pub finger: u8,
    /// Confirmation.
    #[command(flatten)]
    pub confirm: Confirm,
}

/// Extra confirmation for biometric data.
#[derive(Debug, Clone, Args)]
pub struct Biometric {
    /// Confirmation.
    #[command(flatten)]
    pub confirm: Confirm,
    /// Acknowledge that templates are biometric personal data.
    #[arg(long)]
    pub i_understand_biometric_data: bool,
}

/// `templates`.
#[derive(Debug, Subcommand)]
pub enum TemplatesCommand {
    /// Write every fingerprint template, with its user, to a file (mode
    /// 0600).
    Export {
        /// The file.
        #[arg(long, value_name = "FILE")]
        out: PathBuf,
        /// Confirmation.
        #[command(flatten)]
        biometric: Biometric,
    },
    /// Destructive: upload users and templates from an export file,
    /// overwriting users with the same uid.
    Import {
        /// The file.
        file: PathBuf,
        /// Confirmation.
        #[command(flatten)]
        biometric: Biometric,
    },
}

/// `data`.
#[derive(Debug, Subcommand)]
pub enum DataCommand {
    /// Destructive and irreversible: erase users, templates and
    /// attendance. Run `attendance pull` and `templates export` first.
    Clear(ClearArgs),
}

/// `device`.
#[derive(Debug, Subcommand)]
pub enum DeviceCommand {
    /// Add or replace a named device, from `--host`, `--port`,
    /// `--transport` and `--timeout`. Passwords are never stored in the
    /// file; see `set-password`.
    Add {
        /// Name.
        name: String,
    },
    /// List named devices.
    List,
    /// Remove a named device (and its keyring password).
    Remove {
        /// Name.
        name: String,
    },
    /// Show one named device.
    Show {
        /// Name.
        name: String,
    },
    /// Store a device's password in the OS keyring (prompted for).
    SetPassword {
        /// Name.
        name: String,
    },
}

/// `diagnose`.
#[derive(Debug, Clone, Args)]
pub struct DiagnoseArgs {
    /// Device host name or address (or use `--host` or `-d`).
    pub host_arg: Option<String>,
    /// Include the serial number and MAC address in the report.
    #[arg(long)]
    pub include_identifiers: bool,
    /// Write scrubbed packet captures (tests/fixtures/captures format) to
    /// this directory. Captures are always scrubbed.
    #[arg(long, value_name = "DIR")]
    pub capture: Option<PathBuf>,
    /// After the table probes, subscribe to live events and report their
    /// flags and payload lengths (never payloads), and whether the device
    /// answers requests while subscribed. Punch in on the device meanwhile.
    #[arg(long)]
    pub probe_live: bool,
    /// How long the live probe listens, in seconds.
    #[arg(long, default_value_t = 60, requires = "probe_live")]
    pub probe_live_secs: u64,
    /// Write the report to this file instead of standard output.
    #[arg(long = "out", short = 'o', value_name = "FILE")]
    pub out: Option<PathBuf>,
}
