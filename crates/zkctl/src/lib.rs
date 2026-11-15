//! `zkctl`: command-line tool for `ZKTeco` devices, built on
//! [`zkteco_client`].
//!
//! Only `zkctl diagnose` exists so far. It is read-only: every connection it
//! opens has [`ClientConfig::read_only`](zkteco_client::ClientConfig::read_only)
//! set, so the client refuses any command outside
//! [`READ_ONLY_COMMANDS`](zkteco_client::READ_ONLY_COMMANDS) before it is
//! sent.

pub mod capture;
pub mod diagnose;
pub mod live_probe;
pub mod streams;
