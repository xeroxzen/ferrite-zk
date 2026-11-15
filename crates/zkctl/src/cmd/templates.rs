//! `zkctl templates export|import`: fingerprint templates are biometric
//! personal data. Both need `--yes` and `--i-understand-biometric-data`;
//! export files are written with mode 0600 and never printed.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use zkteco_client::{Privilege, Template, Uid, User, UserId};

use crate::cli::{Biometric, Format, Global, TemplatesCommand};
use crate::exit::{Exit, Fail};
use crate::output::{self, privilege_name};
use crate::target::{self, Access, Target};

const FORMAT: &str = "zkctl-templates";
const VERSION: u32 = 1;

const WARNING: &str = "\
WARNING: fingerprint templates are biometric personal data. Many laws (GDPR
article 9, BIPA and others) restrict collecting, storing and moving them.
Keep the file encrypted at rest, share it with no one who does not need it,
and delete it when done.";

/// An export file.
#[derive(Debug, Serialize, Deserialize)]
struct ExportFile {
    format: String,
    version: u32,
    device: String,
    exported_at: String,
    entries: Vec<Entry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Entry {
    user: UserRecord,
    templates: Vec<TemplateRecord>,
}

#[derive(Debug, Serialize, Deserialize)]
struct UserRecord {
    uid: u16,
    user_id: String,
    name: String,
    /// The PIN, needed to recreate the user unchanged.
    password: String,
    privilege: String,
    disabled: bool,
    card: u32,
    group_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct TemplateRecord {
    fid: u8,
    valid: u8,
    data_hex: String,
}

pub(super) async fn run(g: &Global, cmd: TemplatesCommand) -> Result<Exit> {
    let target = target::single(g)?;
    match cmd {
        TemplatesCommand::Export { out, biometric } => {
            confirm(&biometric, "templates export")?;
            export(g, &target, &out, biometric.confirm.dry_run).await
        }
        TemplatesCommand::Import { file, biometric } => {
            confirm(&biometric, "templates import")?;
            import(g, &target, &file, biometric.confirm.dry_run).await
        }
    }
}

fn confirm(b: &Biometric, what: &str) -> Result<()> {
    super::require_yes(&b.confirm, what)?;
    if !b.i_understand_biometric_data {
        return Err(Fail::usage(format!(
            "{what} handles biometric data; pass --i-understand-biometric-data as well"
        )));
    }
    eprintln!("{WARNING}");
    Ok(())
}

async fn export(g: &Global, t: &Target, out: &Path, dry_run: bool) -> Result<Exit> {
    let mut client = target::connect(t, Access::ReadOnly).await?;
    let users = client.users().await?;
    let templates = client.templates().await?;
    if let Err(e) = client.disconnect().await {
        tracing::warn!(%e, "disconnect");
    }
    let mut exit = super::attendance::report_rejected(&t.name, "user", &users.rejected).worst(
        super::attendance::report_rejected(&t.name, "template", &templates.rejected),
    );
    let (entries, orphans) = group(&users.records, templates.records);
    if orphans > 0 {
        eprintln!(
            "warning: {}: {orphans} templates belong to no user and were left out",
            t.name
        );
        exit = exit.worst(Exit::Partial);
    }
    let count: usize = entries.iter().map(|e| e.templates.len()).sum();
    if dry_run {
        let message = format!(
            "dry run: would write {count} templates of {} users to {}",
            entries.len(),
            out.display()
        );
        summary(g.output, &message)?;
        return Ok(exit);
    }
    let file = ExportFile {
        format: FORMAT.into(),
        version: VERSION,
        device: t.name.clone(),
        exported_at: output::host_time(std::time::SystemTime::now()),
        entries,
    };
    crate::files::write_atomic(out, &serde_json::to_vec_pretty(&file)?)?;
    summary(
        g.output,
        &format!("wrote {count} templates to {} (mode 0600)", out.display()),
    )?;
    Ok(exit)
}

/// Templates grouped under their users, and the number with no user.
fn group(users: &[User], templates: Vec<Template>) -> (Vec<Entry>, usize) {
    let mut by_uid: BTreeMap<Uid, Vec<TemplateRecord>> = BTreeMap::new();
    for t in templates {
        by_uid.entry(t.uid).or_default().push(TemplateRecord {
            fid: t.fid,
            valid: t.valid,
            data_hex: hex::encode(&t.data),
        });
    }
    let mut entries = Vec::new();
    for u in users {
        if let Some(templates) = by_uid.remove(&u.uid) {
            entries.push(Entry {
                user: UserRecord {
                    uid: u.uid.0,
                    user_id: u.user_id.as_str().to_owned(),
                    name: u.name.clone(),
                    password: u.password.clone(),
                    privilege: privilege_name(u.privilege),
                    disabled: u.disabled,
                    card: u.card,
                    group_id: u.group_id.clone(),
                },
                templates,
            });
        }
    }
    (entries, by_uid.values().map(Vec::len).sum())
}

async fn import(g: &Global, t: &Target, path: &Path, dry_run: bool) -> Result<Exit> {
    let text = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let file: ExportFile =
        serde_json::from_slice(&text).with_context(|| format!("parsing {}", path.display()))?;
    if file.format != FORMAT || file.version != VERSION {
        return Err(Fail::usage(format!(
            "{} is not a {FORMAT} version {VERSION} file",
            path.display()
        )));
    }
    let entries = file.entries.iter().map(to_client).collect::<Result<Vec<_>>>()?;
    let mut client = target::connect(t, Access::Write { dry_run }).await?;
    client.save_user_templates(&entries).await?;
    if dry_run {
        // The payloads are the templates themselves: never printed.
        output::print_planned(g.output, &client.take_planned(), true)?;
    } else {
        let count: usize = entries.iter().map(|(_, ts)| ts.len()).sum();
        summary(
            g.output,
            &format!("imported {count} templates for {} users", entries.len()),
        )?;
    }
    if let Err(e) = client.disconnect().await {
        tracing::warn!(%e, "disconnect");
    }
    Ok(Exit::Ok)
}

fn to_client(e: &Entry) -> Result<(User, Vec<Template>)> {
    let u = &e.user;
    let privilege = match u.privilege.as_str() {
        "user" => Privilege::User,
        "enroller" => Privilege::Enroller,
        "manager" => Privilege::Manager,
        "admin" => Privilege::Admin,
        other => {
            return Err(Fail::usage(format!(
                "user {}: role {other:?} cannot be written",
                u.user_id
            )))
        }
    };
    let mut user = User::new(Uid(u.uid), UserId(u.user_id.clone()));
    user.name.clone_from(&u.name);
    user.password.clone_from(&u.password);
    user.privilege = privilege;
    user.disabled = u.disabled;
    user.card = u.card;
    user.group_id.clone_from(&u.group_id);
    let templates = e
        .templates
        .iter()
        .map(|t| {
            let data =
                hex::decode(&t.data_hex).with_context(|| format!("user {}: template data", u.user_id))?;
            let mut template = Template::new(Uid(u.uid), t.fid, data.into());
            template.valid = t.valid;
            Ok(template)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((user, templates))
}

fn summary(format: Format, message: &str) -> Result<()> {
    match format {
        Format::Human => output::text(message),
        _ => output::json(&serde_json::json!({ "ok": true, "message": message })),
    }
}
