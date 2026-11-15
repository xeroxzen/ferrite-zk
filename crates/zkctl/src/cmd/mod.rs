//! Command implementations. Each is a thin layer over `zkteco-client`.

mod attendance;
mod cursor;
mod danger;
mod device;
mod diagnose;
mod enroll;
mod info;
mod live;
mod templates;
mod time;
mod users;

use anyhow::Result;
use clap::CommandFactory;

use crate::cli::{Cli, Command, Confirm, Format, Global};
use crate::exit::{Exit, Fail};

/// Run a parsed command line.
///
/// # Errors
///
/// Any failure; [`crate::exit::classify`] maps it to an exit code.
pub async fn run(cli: Cli) -> Result<Exit> {
    if matches!(cli.command, Command::Live(_)) {
        // `live` stops cleanly on Ctrl-C itself.
        return dispatch(cli).await;
    }
    tokio::select! {
        result = dispatch(cli) => result,
        signal = tokio::signal::ctrl_c() => {
            signal?;
            Err(Fail::aborted("interrupted"))
        }
    }
}

async fn dispatch(cli: Cli) -> Result<Exit> {
    let g = &cli.global;
    match cli.command {
        Command::Info(args) => info::run(g, &args).await,
        Command::Time(cmd) => time::run(g, cmd).await,
        Command::Users(cmd) => users::run(g, cmd).await,
        Command::Attendance(cmd) => attendance::run(g, cmd).await,
        Command::Live(args) => live::run(g, &args).await,
        Command::Enroll(args) => enroll::run(g, &args).await,
        Command::Templates(cmd) => templates::run(g, cmd).await,
        Command::Data(cmd) => danger::data(g, cmd).await,
        Command::Restart(c) => danger::power(g, &c, false).await,
        Command::Poweroff(c) => danger::power(g, &c, true).await,
        Command::Device(cmd) => device::run(g, cmd).await,
        Command::Diagnose(args) => diagnose::run(g, &args).await,
        Command::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "zkctl", &mut std::io::stdout());
            Ok(Exit::Ok)
        }
    }
}

/// Refuse a destructive command without `--yes` (a dry run needs none).
fn require_yes(confirm: &Confirm, what: &str) -> Result<()> {
    if confirm.yes || confirm.dry_run {
        return Ok(());
    }
    Err(Fail::usage(format!(
        "{what} changes the device. Nothing was done: pass --yes to proceed, or --dry-run to see what would be sent"
    )))
}

/// Refuse `ndjson` for commands with a single result.
fn one_shot(g: &Global) -> Result<()> {
    if g.output == Format::Ndjson {
        return Err(Fail::usage("this command has a single result: use --output json"));
    }
    Ok(())
}
