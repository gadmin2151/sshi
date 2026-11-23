use crate::state::State;
use anyhow::{Context, Result, ensure};
use clap::Subcommand;
use iroh::EndpointId;
use iroh_ssh::profiles::Profile;
use std::{env, ffi::OsString, os::unix::process::CommandExt, process::Command};

pub const PROFILE_RELAY: &str = "SSHI_PROFILE_RELAY_URL";

#[derive(Subcommand)]
pub enum Profiles {
    /// Show named connections.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Save a new connection, or edit one using its previous name.
    Save {
        #[arg(long, allow_hyphen_values = true)]
        name: String,
        #[arg(long)]
        user: String,
        #[arg(long)]
        endpoint: EndpointId,
        #[arg(long, default_value = "")]
        relay: String,
        #[arg(long, allow_hyphen_values = true)]
        previous: Option<String>,
    },
    /// Remove a named connection.
    Remove {
        #[arg(long, allow_hyphen_values = true)]
        name: String,
    },
    /// Run another sshi command with this connection's relay settings.
    Run {
        #[arg(long, allow_hyphen_values = true)]
        name: String,
        #[arg(last = true, required = true)]
        args: Vec<OsString>,
    },
}

fn find<'a>(profiles: &'a [Profile], name: &str) -> Option<&'a Profile> {
    let folded = name.to_lowercase();
    profiles.iter().find(|p| p.name.to_lowercase() == folded)
}

pub fn lookup(name: &str) -> Result<Profile> {
    find(&State::load()?.library()?.connections, name)
        .cloned()
        .with_context(|| format!("no connection named '{name}'"))
}

pub fn command(profile: &Profile) -> Result<Command> {
    let mut command = Command::new(env::current_exe()?);
    if profile.relay.is_empty() {
        command.env_remove(PROFILE_RELAY);
    } else {
        command.env(PROFILE_RELAY, &profile.relay);
    }
    Ok(command)
}

pub fn run(action: Profiles) -> Result<()> {
    let state = State::load()?;
    match action {
        Profiles::List { json } => {
            let profiles = state.library()?.connections;
            if json {
                println!("{}", serde_json::to_string(&profiles)?);
            } else {
                for p in profiles {
                    println!("{}\t{}", p.name, p.destination());
                }
            }
            Ok(())
        }
        Profiles::Save {
            name,
            user,
            endpoint,
            relay,
            previous,
        } => {
            let profile = Profile {
                name,
                user,
                endpoint,
                relay,
            };
            profile.validate()?;
            state.update_library(|library| {
                if let Some(old) = previous {
                    let idx = library
                        .connections
                        .iter()
                        .position(|p| p.name.to_lowercase() == old.to_lowercase())
                        .with_context(|| format!("no connection named '{old}'"))?;
                    library.connections.remove(idx);
                    ensure!(
                        find(&library.connections, &profile.name).is_none(),
                        "connection name already in use"
                    );
                    library.connections.insert(idx, profile);
                } else {
                    ensure!(
                        find(&library.connections, &profile.name).is_none(),
                        "connection name already in use; use --previous to edit"
                    );
                    library.connections.push(profile);
                }
                Ok(())
            })
        }
        Profiles::Remove { name } => state.update_library(|library| {
            let old = library.connections.len();
            library
                .connections
                .retain(|p| p.name.to_lowercase() != name.to_lowercase());
            ensure!(
                old != library.connections.len(),
                "no connection named '{name}'"
            );
            Ok(())
        }),
        Profiles::Run { name, args } => {
            let p = lookup(&name)?;
            Err(command(&p)?.args(args).exec()).context("run sshi")
        }
    }
}

pub fn connect(name: &str, remote_command: Vec<OsString>) -> Result<()> {
    let p = lookup(name)?;
    Err(command(&p)?
        .arg("--")
        .arg(p.destination())
        .args(remote_command)
        .exec())
    .context("run saved connection")
}

pub fn invite(name: &str, client: EndpointId, service_user: Option<&str>) -> Result<()> {
    let p = lookup(name)?;
    let service_user = service_user.filter(|u| *u != p.user);
    let remote = iroh_ssh::issue_invitation_command(p.endpoint, client, service_user)?;
    Err(command(&p)?
        .arg("-T")
        .arg("--")
        .arg(p.destination())
        .arg(remote)
        .exec())
    .context("issue invitation over iroh")
}
