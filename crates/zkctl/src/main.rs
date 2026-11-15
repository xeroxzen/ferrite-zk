//! `zkctl`: command-line tool for `ZKTeco` devices.

use std::process::ExitCode;

use clap::Parser;
use zkctl::cli::Cli;
use zkctl::exit::{classify, Exit};

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            // Help and version print to stdout and exit 0; real usage
            // errors exit 2.
            let code = if e.use_stderr() { Exit::Usage } else { Exit::Ok };
            if let Err(print) = e.print() {
                eprintln!("{print}");
            }
            return ExitCode::from(code.code());
        }
    };
    init_logging(cli.global.verbose, cli.global.quiet);
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: starting the runtime: {e}");
            return ExitCode::from(Exit::Error.code());
        }
    };
    match runtime.block_on(zkctl::cmd::run(cli)) {
        Ok(exit) => ExitCode::from(exit.code()),
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(classify(&e).code())
        }
    }
}

fn init_logging(verbose: u8, quiet: bool) {
    let level = match (quiet, verbose) {
        (true, _) => "error",
        (false, 0) => "warn",
        (false, 1) => "info",
        (false, 2) => "debug",
        (false, _) => "trace",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level));
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .init();
}
