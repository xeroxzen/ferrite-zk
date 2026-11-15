//! `zkctl enroll`.

use std::time::Duration;

use anyhow::Result;
use serde::Serialize;
use zkteco_client::{EnrollOptions, EnrollOutcome, EnrollReport, UserId};

use crate::cli::{EnrollArgs, Format, Global};
use crate::exit::Exit;
use crate::output;
use crate::target::{self, Access};

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Line {
    Progress {
        report: String,
    },
    Outcome {
        outcome: String,
        scans: Option<u8>,
        size: Option<u16>,
        position: Option<u16>,
    },
}

pub(super) async fn run(g: &Global, args: &EnrollArgs) -> Result<Exit> {
    super::require_yes(&args.confirm, "enroll")?;
    let target = target::single(g)?;
    let dry_run = args.confirm.dry_run;
    let mut client = target::connect(&target, Access::Write { dry_run }).await?;
    if !dry_run && g.output == Format::Human {
        eprintln!(
            "Place finger {} on the sensor of {} when it asks.",
            args.finger, target.name
        );
    }
    let mut opts = EnrollOptions::default();
    if dry_run {
        // A dry run gets no reports; do not wait for them.
        opts = opts.report_timeout(Duration::ZERO);
    }
    let format = g.output;
    let mut failed_print = None;
    let outcome = client
        .enroll_user(&UserId(args.user_id.clone()), args.finger, opts, |r| {
            if let Err(e) = progress(format, r) {
                failed_print.get_or_insert(e);
            }
        })
        .await?;
    if let Some(e) = failed_print {
        return Err(e);
    }
    if dry_run {
        output::print_planned(g.output, &client.take_planned(), false)?;
    } else {
        print_outcome(g.output, outcome)?;
    }
    if let Err(e) = client.disconnect().await {
        tracing::warn!(%e, "disconnect");
    }
    Ok(match outcome {
        EnrollOutcome::Stored { .. } => Exit::Ok,
        _ if dry_run => Exit::Ok,
        _ => Exit::Device,
    })
}

fn report_name(r: EnrollReport) -> String {
    match r {
        EnrollReport::ScanAccepted => "scan_accepted".into(),
        EnrollReport::Stored { .. } => "stored".into(),
        EnrollReport::Failed => "failed".into(),
        EnrollReport::Duplicate => "duplicate".into(),
        EnrollReport::TimedOut => "timed_out".into(),
        EnrollReport::Other(code) => format!("code:{code}"),
    }
}

fn progress(format: Format, r: EnrollReport) -> Result<()> {
    match format {
        Format::Ndjson => output::ndjson(&Line::Progress {
            report: report_name(r),
        }),
        Format::Json => Ok(()),
        Format::Human => output::text(&match r {
            EnrollReport::ScanAccepted => "scan accepted; place the same finger again".to_owned(),
            other => format!("device: {}", report_name(other)),
        }),
    }
}

fn print_outcome(format: Format, outcome: EnrollOutcome) -> Result<()> {
    let line = match outcome {
        EnrollOutcome::Stored {
            size,
            position,
            scans,
        } => Line::Outcome {
            outcome: "stored".into(),
            scans: Some(scans),
            size,
            position,
        },
        other => {
            let (name, scans) = match other {
                EnrollOutcome::Duplicate => ("duplicate", None),
                EnrollOutcome::Failed => ("failed", None),
                EnrollOutcome::TimedOut => ("timed_out", None),
                EnrollOutcome::Unexpected { scans, .. } => ("unexpected", Some(scans)),
                EnrollOutcome::NoReport { scans } => ("no_report", Some(scans)),
                EnrollOutcome::Stored { .. } => ("stored", None),
            };
            Line::Outcome {
                outcome: name.into(),
                scans,
                size: None,
                position: None,
            }
        }
    };
    match format {
        Format::Json => output::json(&line),
        Format::Ndjson => output::ndjson(&line),
        Format::Human => {
            let Line::Outcome { outcome, .. } = &line else {
                return Ok(());
            };
            output::text(&format!("enrolment: {outcome}"))
        }
    }
}
