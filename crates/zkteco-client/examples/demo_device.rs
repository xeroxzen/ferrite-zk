//! A demo device, to try `zkctl` or the library without hardware.
//!
//! The mock device from this workspace's tests (`zkteco_client::demo`),
//! seeded with ten fictional users, punching every few seconds, and
//! dropping off the network now and then while it keeps recording punches,
//! so a client with `--reconnect --backfill` shows backfilled events.
//! Listens on 127.0.0.1 only. Every name and punch is made up.
//!
//! ```sh
//! cargo run -p zkteco-client --example demo_device -- --port 14370 --name front
//! ```

use std::future::Future;
use std::net::{Ipv4Addr, SocketAddr};
use std::process::ExitCode;
use std::time::Duration;

use chrono::{NaiveDateTime, Timelike};
use tokio::time::Instant;
use zkteco_client::demo::{users, DeviceState, MockDevice};
use zkteco_client::{Attendance, ClientConfig};
use zkteco_proto::records::attendance::{AttendanceStatus, Punch};

/// Obviously fictional people, user IDs 1001 to 1010.
const NAMES: [&str; 10] = [
    "Ada Example",
    "Bo Placeholder",
    "Cy Sample",
    "Di Fictional",
    "Ed Testcase",
    "Flo Mockley",
    "Gus Dummy",
    "Hal Stub",
    "Ivy Fixture",
    "Jo Notreal",
];

/// How often punches are recorded before the demo starts, to give
/// `attendance pull` something to show.
const HISTORY: usize = 12;

/// How the demo device behaves.
#[derive(Debug, Clone)]
pub struct Options {
    /// TCP and UDP port on 127.0.0.1; 0 picks a free one.
    pub port: u16,
    /// Profile name to suggest; also the device name, and in the serial
    /// number (`DEMO-<NAME>`), so two demo devices have different ids.
    pub name: String,
    /// Mean time between punches; each wait is between 2/3 and 4/3 of it.
    pub interval: Duration,
    /// How often the device drops off the network; `None` never.
    pub drop_every: Option<Duration>,
    /// How long each drop lasts.
    pub drop_for: Duration,
    /// Seed for who punches, how, and when.
    pub seed: u64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            port: 14370,
            name: "front".into(),
            interval: Duration::from_secs(3),
            drop_every: Some(Duration::from_secs(30)),
            drop_for: Duration::from_secs(5),
            seed: 1,
        }
    }
}

/// A running demo device.
pub struct Demo {
    device: MockDevice,
    opts: Options,
    rng: SplitMix64,
    /// Whether each user's next punch is a check-out.
    checked_in: [bool; NAMES.len()],
}

/// Start listening, with the users and some earlier punches in place.
///
/// # Errors
///
/// The port cannot be bound (TCP or UDP).
pub async fn start(opts: Options) -> std::io::Result<Demo> {
    let mut demo_users = users(10);
    for (u, name) in demo_users.iter_mut().zip(NAMES) {
        name.clone_into(&mut u.name);
    }
    let mut state = DeviceState {
        users: demo_users,
        time: now(),
        firmware: "Ver 6.60 Ferrite demo".into(),
        ..DeviceState::default()
    };
    set_option(
        &mut state,
        "~SerialNumber",
        &format!("DEMO-{}", opts.name.to_uppercase()),
    );
    set_option(&mut state, "~DeviceName", &opts.name);
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, opts.port));
    let device = MockDevice::start_on(addr, state).await?;
    let mut demo = Demo {
        device,
        rng: SplitMix64(opts.seed),
        opts,
        checked_in: [false; NAMES.len()],
    };
    demo.record_history();
    Ok(demo)
}

impl Demo {
    /// Where the device listens.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.device.addr
    }

    /// A client configuration for this device.
    #[must_use]
    pub fn config(&self) -> ClientConfig {
        self.device.config()
    }

    /// Punch and drop until `stop` completes, describing each step to
    /// `report`.
    pub async fn run(mut self, stop: impl Future<Output = ()>, mut report: impl FnMut(String)) {
        let mut stop = std::pin::pin!(stop);
        let started = Instant::now();
        let mut next_punch = started + self.wait();
        let mut next_drop = self.opts.drop_every.map(|d| started + d);
        let mut back_at: Option<Instant> = None;
        // Runs until `stop`: a demo has no natural end. Each turn sleeps
        // at most a second, to keep the device clock current.
        loop {
            let wake = [
                Some(next_punch),
                next_drop,
                back_at,
                Some(Instant::now() + Duration::from_secs(1)),
            ]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or_else(Instant::now);
            tokio::select! {
                () = &mut stop => return,
                () = tokio::time::sleep_until(wake) => {}
            }
            let at = Instant::now();
            self.device.state().time = now();
            if back_at.is_some_and(|t| at >= t) {
                back_at = None;
                self.device.set_offline(false);
                next_drop = self.opts.drop_every.map(|d| at + d);
                report("back online".into());
            } else if next_drop.is_some_and(|t| at >= t) {
                next_drop = None;
                back_at = Some(at + self.opts.drop_for);
                self.device.set_offline(true);
                report(format!(
                    "offline for {} s, still recording punches",
                    self.opts.drop_for.as_secs()
                ));
            }
            if at >= next_punch {
                next_punch = at + self.wait();
                report(self.punch(back_at.is_some()));
            }
        }
    }

    /// One punch, now: stored in the log, and sent as a live event if a
    /// client is subscribed and the device is online.
    fn punch(&mut self, offline: bool) -> String {
        let (record, who) = self.next_record(now());
        let subscribed = self.device.state().event_masks.last().is_some_and(|m| *m != 0);
        let how = if offline {
            self.device.state().attendance.push(record.clone());
            "recorded offline"
        } else if subscribed {
            // An event nobody took (its client went away) is stale.
            self.device.state().outbox.clear();
            self.device.push_punch(record.clone());
            "live"
        } else {
            self.device.state().attendance.push(record.clone());
            "recorded"
        };
        format!(
            "{} {who} {:?} {:?} ({how})",
            record.user_id.as_ref().map_or("?", |id| id.as_str()),
            record.status,
            record.punch
        )
    }

    /// Earlier punches, a few minutes apart, ending now.
    fn record_history(&mut self) {
        let end = now();
        for i in (1..=HISTORY).rev() {
            let minutes = i64::try_from(i * 7).unwrap_or(i64::MAX);
            let (record, _) = self.next_record(end - chrono::Duration::minutes(minutes));
            self.device.state().attendance.push(record);
        }
    }

    /// The next punch: a random user, alternating check-in and check-out.
    fn next_record(&mut self, timestamp: NaiveDateTime) -> (Attendance, &'static str) {
        let i = self.rng.below(NAMES.len());
        let status = [
            AttendanceStatus::Fingerprint,
            AttendanceStatus::Fingerprint,
            AttendanceStatus::Face,
            AttendanceStatus::Card,
            AttendanceStatus::Password,
        ][self.rng.below(5)];
        let punch = if self.checked_in[i] {
            Punch::CheckOut
        } else {
            Punch::CheckIn
        };
        self.checked_in[i] = !self.checked_in[i];
        let (uid, user_id) = {
            let state = self.device.state();
            (state.users[i].uid, state.users[i].user_id.clone())
        };
        let record = Attendance {
            uid: Some(uid),
            user_id: Some(user_id),
            timestamp,
            status,
            punch,
            workcode: None,
        };
        (record, NAMES[i])
    }

    /// A wait between 2/3 and 4/3 of the interval.
    fn wait(&mut self) -> Duration {
        let third = self.opts.interval / 3;
        let jitter = u32::try_from(self.rng.below(1001)).unwrap_or(0);
        third * 2 + third * 2 * jitter / 1000
    }
}

/// The local time to the second, as a device keeps it.
fn now() -> NaiveDateTime {
    let t = chrono::Local::now().naive_local();
    t.with_nanosecond(0).unwrap_or(t)
}

fn set_option(state: &mut DeviceState, key: &str, value: &str) {
    state.options.retain(|(k, _)| k != key);
    state.options.push((key.to_owned(), value.to_owned()));
}

/// A small deterministic generator (SplitMix64): enough for a demo, and no
/// extra dependency.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform enough in `0..n`; `n` must be non-zero.
    fn below(&mut self, n: usize) -> usize {
        assert!(n > 0, "below(0)");
        let n = u64::try_from(n).unwrap_or(u64::MAX);
        usize::try_from(self.next() % n).unwrap_or(0)
    }
}

const USAGE: &str = "\
Usage: demo_device [--port N] [--name NAME] [--interval SECS] [--drop-every SECS]
                   [--seed N] [--no-drops]

  --port N           TCP and UDP port on 127.0.0.1 (default 14370)
  --name NAME        profile and device name (default front)
  --interval SECS    mean time between punches (default 3)
  --drop-every SECS  go offline for 5 s this often (default 30)
  --seed N           the same seed gives the same people and punches (default 1)
  --no-drops         never go offline";

/// Options from the command line; `Err` holds the message to print.
fn parse(args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut opts = Options::default();
    let mut args = args.peekable();
    // Bounded: each turn consumes at least one argument.
    while let Some(flag) = args.next() {
        if flag == "--no-drops" {
            opts.drop_every = None;
            continue;
        }
        if flag == "-h" || flag == "--help" {
            return Err(USAGE.into());
        }
        let value = args
            .next()
            .ok_or_else(|| format!("{flag} needs a value\n\n{USAGE}"))?;
        let bad = |what: &str| format!("{flag} {value}: {what}\n\n{USAGE}");
        match flag.as_str() {
            "--port" => opts.port = value.parse().map_err(|_| bad("not a port"))?,
            "--name" => {
                let ok = !value.is_empty()
                    && value
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
                if !ok {
                    return Err(bad("letters, digits, - and _ only"));
                }
                opts.name = value;
            }
            "--interval" => {
                let secs: f64 = value.parse().map_err(|_| bad("not a number"))?;
                opts.interval = Duration::try_from_secs_f64(secs)
                    .ok()
                    .filter(|d| *d >= Duration::from_millis(100))
                    .ok_or_else(|| bad("at least 0.1 seconds"))?;
            }
            "--drop-every" => {
                let secs: u64 = value.parse().map_err(|_| bad("not a whole number of seconds"))?;
                if Duration::from_secs(secs) <= opts.drop_for {
                    return Err(bad("must be longer than the 5 s drop"));
                }
                opts.drop_every = Some(Duration::from_secs(secs));
            }
            "--seed" => opts.seed = value.parse().map_err(|_| bad("not a number"))?,
            _ => return Err(format!("unknown option {flag}\n\n{USAGE}")),
        }
    }
    Ok(opts)
}

fn banner(opts: &Options, addr: SocketAddr) -> String {
    let drops = match opts.drop_every {
        Some(d) => format!(
            "offline for {} s every {} s",
            opts.drop_for.as_secs(),
            d.as_secs()
        ),
        None => "never offline".into(),
    };
    let name = &opts.name;
    format!(
        "Demo device {name:?} on {addr} (TCP and UDP), serial DEMO-{upper}.\n\
         Fictional people and punches. A punch every ~{interval:.1} s; {drops}.\n\
         \n\
         Register it and stream from it, in another terminal:\n\
         \n  zkctl device add {name} --host {ip} --port {port} --transport tcp\n\
         \x20 zkctl live -d {name} --reconnect --backfill\n\
         \n\
         Ctrl-C to stop.\n",
        upper = name.to_uppercase(),
        interval = opts.interval.as_secs_f64(),
        ip = addr.ip(),
        port = addr.port(),
    )
}

#[tokio::main]
async fn main() -> ExitCode {
    let opts = match parse(std::env::args().skip(1)) {
        Ok(opts) => opts,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    let demo = match start(opts.clone()).await {
        Ok(demo) => demo,
        Err(e) => {
            eprintln!("cannot listen on 127.0.0.1:{}: {e}", opts.port);
            return ExitCode::FAILURE;
        }
    };
    println!("{}", banner(&opts, demo.addr()));
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            eprintln!("cannot listen for Ctrl-C: {e}");
        }
    };
    let clock = || chrono::Local::now().format("%H:%M:%S");
    demo.run(ctrl_c, |line| println!("{}  {line}", clock())).await;
    ExitCode::SUCCESS
}
