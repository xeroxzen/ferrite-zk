//! `zkctl`: command-line tool for `ZKTeco` devices, built on
//! [`zkteco_client`]. The binary is a thin layer over this library; the
//! library is public so that `xtask` can generate man pages and completions
//! from [`cli::Cli`], and so the commands can be tested.
//!
//! Read commands connect with
//! [`ClientConfig::read_only`](zkteco_client::ClientConfig::read_only), so
//! the client refuses any command outside
//! [`READ_ONLY_COMMANDS`](zkteco_client::READ_ONLY_COMMANDS) before it is
//! sent. Only explicitly destructive commands build a writable client, and
//! only with `--yes` (or `--dry-run`, which sends no writes).

#![deny(missing_docs)]

pub mod capture;
pub mod cli;
pub mod cmd;
pub mod diagnose;
pub mod duration;
pub mod event_id;
pub mod exit;
pub mod files;
pub mod live_probe;
pub mod output;
pub mod profiles;
pub mod secrets;
pub mod streams;
pub mod target;
