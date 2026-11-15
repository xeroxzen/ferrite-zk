//! `zkctl`: command-line tool for `ZKTeco` devices.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use zkctl::diagnose::{self, Options};
use zkteco_client::{Transport, DEFAULT_PORT};

#[derive(Parser)]
#[command(version, about = "Command-line tool for ZKTeco devices")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Survey a device without changing it, and print a JSON report shaped
    /// like a docs/COMPATIBILITY.md row. Read-only: the client refuses any
    /// command outside its read-only allowlist.
    Diagnose(DiagnoseArgs),
}

#[derive(clap::Args)]
struct DiagnoseArgs {
    /// Device host name or address.
    host: String,
    /// Device port.
    #[arg(long, default_value_t = DEFAULT_PORT)]
    port: u16,
    /// Comm key (communication password). Prefer the environment variable,
    /// which stays out of shell history.
    #[arg(long, env = "ZK_PASSWORD", hide_env_values = true)]
    password: Option<u32>,
    /// Transport for the main connection. Table reads are probed over TCP
    /// and UDP either way.
    #[arg(long, value_enum, default_value_t = TransportArg::Auto)]
    transport: TransportArg,
    /// Per-reply timeout in milliseconds.
    #[arg(long, default_value_t = 5000)]
    timeout_ms: u64,
    /// Include the serial number and MAC address in the report.
    #[arg(long)]
    include_identifiers: bool,
    /// Write scrubbed packet captures (tests/fixtures/captures format) to
    /// this directory. Captures are always scrubbed.
    #[arg(long, value_name = "DIR")]
    capture: Option<PathBuf>,
    /// After the table probes, subscribe to live events and report their
    /// flags and payload lengths (never payloads), and whether the device
    /// answers requests while subscribed. Punch in on the device meanwhile.
    #[arg(long)]
    probe_live: bool,
    /// How long the live probe listens, in seconds.
    #[arg(long, default_value_t = 60, requires = "probe_live")]
    probe_live_secs: u64,
    /// Write the report to this file instead of standard output.
    #[arg(long, short, value_name = "FILE")]
    output: Option<PathBuf>,
}

#[derive(Clone, Copy, ValueEnum)]
enum TransportArg {
    Auto,
    Tcp,
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

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();
    match Cli::parse().command {
        Command::Diagnose(args) => run_diagnose(args).await,
    }
}

async fn run_diagnose(args: DiagnoseArgs) -> Result<()> {
    let opts = Options {
        host: args.host,
        port: args.port,
        password: args.password,
        transport: args.transport.into(),
        timeout: Duration::from_millis(args.timeout_ms),
        include_identifiers: args.include_identifiers,
        capture: args.capture.is_some(),
        probe_live: args.probe_live.then(|| Duration::from_secs(args.probe_live_secs)),
    };
    let outcome = diagnose::run(&opts)
        .await
        .with_context(|| format!("diagnosing {}:{}", opts.host, opts.port))?;
    if let Some(dir) = &args.capture {
        write_captures(dir, &outcome.captures)?;
    }
    let json = serde_json::to_string_pretty(&outcome.report)?;
    match &args.output {
        Some(path) => {
            std::fs::write(path, json + "\n").with_context(|| format!("writing {}", path.display()))?;
        }
        None => println!("{json}"),
    }
    Ok(())
}

fn write_captures(dir: &Path, captures: &[(String, serde_json::Value)]) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    for (name, capture) in captures {
        let path = dir.join(name);
        let text = serde_json::to_string_pretty(capture)?;
        std::fs::write(&path, text + "\n").with_context(|| format!("writing {}", path.display()))?;
        eprintln!("wrote {}", path.display());
    }
    Ok(())
}
