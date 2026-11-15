//! Irreversible commands: `attendance clear`, `data clear`, `restart`,
//! `poweroff`.

use anyhow::Result;

use super::attendance::expect_name;
use crate::cli::{ClearArgs, Confirm, DataCommand, Format, Global};
use crate::exit::Exit;
use crate::output;
use crate::target::{self, Access, Target};

pub(super) async fn data(g: &Global, cmd: DataCommand) -> Result<Exit> {
    let DataCommand::Clear(args) = cmd;
    let target = target::single(g)?;
    clear(g, &target, &args, true).await
}

/// Erase the attendance log, or with `everything` all data.
pub(super) async fn clear(g: &Global, target: &Target, args: &ClearArgs, everything: bool) -> Result<Exit> {
    let what = if everything {
        "data clear (users, templates and attendance)"
    } else {
        "attendance clear"
    };
    super::require_yes(&args.confirm, what)?;
    if !args.confirm.dry_run {
        expect_name(args, &target.name, what)?;
    }
    let access = Access::Write {
        dry_run: args.confirm.dry_run,
    };
    let mut client = target::connect(target, access).await?;
    if everything {
        client.clear_data().await?;
    } else {
        client.clear_attendance().await?;
    }
    if args.confirm.dry_run {
        output::print_planned(g.output, &client.take_planned(), false)?;
    } else {
        done(g.output, &format!("{}: {what} done", target.name))?;
    }
    if let Err(e) = client.disconnect().await {
        tracing::warn!(%e, "disconnect");
    }
    Ok(Exit::Ok)
}

/// Restart, or with `off` power off.
pub(super) async fn power(g: &Global, confirm: &Confirm, off: bool) -> Result<Exit> {
    let what = if off { "poweroff" } else { "restart" };
    super::require_yes(confirm, what)?;
    let target = target::single(g)?;
    let access = Access::Write {
        dry_run: confirm.dry_run,
    };
    let mut client = target::connect(&target, access).await?;
    if off {
        client.poweroff().await?;
    } else {
        client.restart().await?;
    }
    if confirm.dry_run {
        output::print_planned(g.output, &client.take_planned(), false)?;
    } else {
        done(g.output, &format!("{}: {what} sent", target.name))?;
    }
    Ok(Exit::Ok)
}

fn done(format: Format, message: &str) -> Result<()> {
    match format {
        Format::Human => output::text(message),
        _ => output::json(&serde_json::json!({ "ok": true, "message": message })),
    }
}
