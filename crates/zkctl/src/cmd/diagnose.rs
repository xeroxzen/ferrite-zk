//! `zkctl diagnose`, under the shared device options.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::cli::{DiagnoseArgs, Global};
use crate::diagnose::{self, Options};
use crate::exit::Exit;
use crate::target::{self, Access, Target};

pub(super) async fn run(g: &Global, args: &DiagnoseArgs) -> Result<Exit> {
    let target = match &args.host_arg {
        Some(host) => {
            let mut g = g.clone();
            g.host = Some(host.clone());
            g.devices.clear();
            target::single(&g)?
        }
        None => target::single(g)?,
    };
    let opts = options(&target, args).await;
    let outcome = diagnose::run(&opts)
        .await
        .with_context(|| format!("diagnosing {}:{}", opts.host, opts.port))?;
    if let Some(dir) = &args.capture {
        write_captures(dir, &outcome.captures)?;
    }
    let json = serde_json::to_string_pretty(&outcome.report)?;
    match &args.out {
        Some(path) => {
            std::fs::write(path, json + "\n").with_context(|| format!("writing {}", path.display()))?;
        }
        None => crate::output::text(&json)?,
    }
    Ok(Exit::Ok)
}

async fn options(target: &Target, args: &DiagnoseArgs) -> Options {
    let config = target::config_for(target, Access::ReadOnly).await;
    Options {
        host: config.host.clone(),
        port: config.port,
        password: config.password,
        transport: config.transport,
        timeout: config.request_timeout,
        include_identifiers: args.include_identifiers,
        capture: args.capture.is_some(),
        probe_live: args.probe_live.then(|| Duration::from_secs(args.probe_live_secs)),
    }
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
