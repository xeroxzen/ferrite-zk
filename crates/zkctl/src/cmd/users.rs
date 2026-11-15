//! `zkctl users list|add|update|delete`.

use anyhow::{Context, Result};
use zkteco_client::{Client, Privilege, Uid, User, UserId};
use zkteco_proto::records::user::next_free_ids;

use crate::cli::{Confirm, Format, Global, PrivilegeArg, UserFields, UserRef, UsersCommand};
use crate::exit::{Exit, Fail};
use crate::output::{self, UserOut};
use crate::target::{self, Access, Target};

pub(super) async fn run(g: &Global, cmd: UsersCommand) -> Result<Exit> {
    let target = target::single(g)?;
    match cmd {
        UsersCommand::List { filter } => list(g, &target, filter.as_deref()).await,
        UsersCommand::Add {
            user_id,
            uid,
            fields,
            confirm,
        } => {
            super::require_yes(&confirm, "users add")?;
            let mut client = connect_write(&target, &confirm).await?;
            let users = client.users().await?.records;
            let user = new_user(&users, UserId(user_id), uid.map(Uid), &fields)?;
            client.set_user(&user).await?;
            finish(
                g,
                client,
                &confirm,
                &format!("added user {} (uid {})", user.user_id, user.uid.0),
            )
            .await
        }
        UsersCommand::Update {
            user,
            fields,
            confirm,
        } => {
            super::require_yes(&confirm, "users update")?;
            let mut client = connect_write(&target, &confirm).await?;
            let mut existing = find(&mut client, &user).await?;
            apply(&mut existing, &fields)?;
            client.set_user(&existing).await?;
            finish(g, client, &confirm, &format!("updated user {}", existing.user_id)).await
        }
        UsersCommand::Delete { user, confirm } => {
            super::require_yes(&confirm, "users delete")?;
            let mut client = connect_write(&target, &confirm).await?;
            let existing = find(&mut client, &user).await?;
            client.delete_user(existing.uid).await?;
            finish(g, client, &confirm, &format!("deleted user {}", existing.user_id)).await
        }
    }
}

async fn list(g: &Global, target: &Target, filter: Option<&str>) -> Result<Exit> {
    let mut client = target::connect(target, Access::ReadOnly).await?;
    let batch = client.users().await?;
    if let Err(e) = client.disconnect().await {
        tracing::warn!(%e, "disconnect");
    }
    let needle = filter.map(str::to_lowercase);
    let users: Vec<UserOut> = batch
        .records
        .iter()
        .filter(|u| {
            needle
                .as_deref()
                .is_none_or(|n| u.user_id.as_str() == n || u.name.to_lowercase().contains(n))
        })
        .map(UserOut::from)
        .collect();
    match g.output {
        Format::Json => output::json(&users)?,
        Format::Ndjson => {
            for u in &users {
                output::ndjson(u)?;
            }
        }
        Format::Human => {
            let rows: Vec<Vec<String>> = users
                .iter()
                .map(|u| {
                    vec![
                        u.uid.to_string(),
                        u.user_id.clone(),
                        u.name.clone(),
                        u.privilege.clone(),
                        if u.disabled { "yes" } else { "" }.into(),
                        if u.card == 0 {
                            String::new()
                        } else {
                            u.card.to_string()
                        },
                    ]
                })
                .collect();
            output::text(&output::table(
                &["UID", "USER ID", "NAME", "ROLE", "DISABLED", "CARD"],
                &rows,
            ))?;
        }
    }
    Ok(super::attendance::report_rejected(
        &target.name,
        "user",
        &batch.rejected,
    ))
}

async fn connect_write(target: &Target, confirm: &Confirm) -> Result<Client> {
    target::connect(
        target,
        Access::Write {
            dry_run: confirm.dry_run,
        },
    )
    .await
}

/// Print the dry-run plan or the result, then disconnect.
async fn finish(g: &Global, mut client: Client, confirm: &Confirm, done: &str) -> Result<Exit> {
    if confirm.dry_run {
        output::print_planned(g.output, &client.take_planned(), false)?;
    } else {
        match g.output {
            Format::Human => output::text(done)?,
            _ => output::json(&serde_json::json!({ "ok": true, "message": done }))?,
        }
    }
    if let Err(e) = client.disconnect().await {
        tracing::warn!(%e, "disconnect");
    }
    Ok(Exit::Ok)
}

async fn find(client: &mut Client, which: &UserRef) -> Result<User> {
    let users = client.users().await?.records;
    let found = match (which.uid, &which.user_id) {
        (Some(uid), _) => users.into_iter().find(|u| u.uid == Uid(uid)),
        (None, Some(id)) => users.into_iter().find(|u| u.user_id.as_str() == id),
        (None, None) => None,
    };
    found
        .ok_or_else(|| Fail::usage(format!("no such user: {which:?}")))
        .context("finding the user")
}

fn new_user(existing: &[User], user_id: UserId, uid: Option<Uid>, fields: &UserFields) -> Result<User> {
    if existing.iter().any(|u| u.user_id == user_id) {
        return Err(Fail::usage(format!(
            "user ID {user_id} exists; use `users update`"
        )));
    }
    let uid = match uid {
        Some(uid) if existing.iter().any(|u| u.uid == uid) => {
            return Err(Fail::usage(format!("uid {} is taken; use `users update`", uid.0)));
        }
        Some(uid) => uid,
        None => next_free_ids(existing)
            .map(|(uid, _)| uid)
            .ok_or_else(|| Fail::usage("no free uid"))?,
    };
    let mut user = User::new(uid, user_id);
    apply(&mut user, fields)?;
    Ok(user)
}

fn apply(user: &mut User, f: &UserFields) -> Result<()> {
    if matches!(user.privilege, Privilege::Unknown(_)) && f.privilege.is_none() {
        return Err(Fail::usage(
            "this user has a role zkctl cannot write back; pass --privilege to replace it",
        ));
    }
    if let Some(name) = &f.name {
        user.name.clone_from(name);
    }
    if let Some(p) = f.privilege {
        user.privilege = match p {
            PrivilegeArg::User => Privilege::User,
            PrivilegeArg::Enroller => Privilege::Enroller,
            PrivilegeArg::Manager => Privilege::Manager,
            PrivilegeArg::Admin => Privilege::Admin,
        };
    }
    if let Some(pw) = &f.user_password {
        user.password.clone_from(pw);
    }
    if let Some(card) = f.card {
        user.card = card;
    }
    if let Some(group) = &f.group {
        user.group_id.clone_from(group);
    }
    if let Some(disabled) = f.disabled {
        user.disabled = disabled;
    }
    Ok(())
}
