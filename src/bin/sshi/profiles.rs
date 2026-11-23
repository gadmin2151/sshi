use crate::state::State;
use anyhow::{Context, Result, ensure};
use clap::Subcommand;
use iroh::EndpointId;
use iroh_ssh::profiles::Profile;
use std::{
    env,
    ffi::OsString,
    io::Read,
    os::unix::process::CommandExt,
    process::{Command, Stdio},
};

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
    /// Rename a saved connection without changing its settings.
    Rename {
        #[arg(long, allow_hyphen_values = true)]
        name: String,
        #[arg(long, allow_hyphen_values = true)]
        to: String,
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

fn position(profiles: &[Profile], name: &str) -> Option<usize> {
    let folded = name.to_lowercase();
    profiles
        .iter()
        .position(|p| p.name.to_lowercase() == folded)
}

fn profile_index(profiles: &[Profile], name: &str) -> Result<usize> {
    position(profiles, name).with_context(|| format!("no connection named '{name}'"))
}

pub fn lookup(name: &str) -> Result<Profile> {
    let profiles = State::load()?.library()?.connections;
    Ok(profiles[profile_index(&profiles, name)?].clone())
}

pub fn command(profile: &Profile) -> Result<Command> {
    let mut command = Command::new(env::current_exe()?);
    set_relay(&mut command, profile);
    Ok(command)
}

pub fn set_relay(command: &mut Command, profile: &Profile) {
    if profile.relay.is_empty() {
        command.env_remove(PROFILE_RELAY);
    } else {
        command.env(PROFILE_RELAY, &profile.relay);
    }
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
                    let idx = profile_index(&library.connections, &old)?;
                    library.connections[idx] = profile;
                } else {
                    ensure!(
                        position(&library.connections, &profile.name).is_none(),
                        "connection name already in use; use --previous to edit"
                    );
                    library.connections.push(profile);
                }
                Ok(())
            })
        }
        Profiles::Rename { name, to } => state.update_library(|library| {
            let index = profile_index(&library.connections, &name)?;
            library.connections[index].name = to;
            Ok(())
        }),
        Profiles::Remove { name } => state.update_library(|library| {
            let index = profile_index(&library.connections, &name)?;
            library.connections.remove(index);
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

pub fn invite(
    name: &str,
    client: EndpointId,
    service_user: Option<&str>,
    share: bool,
) -> Result<()> {
    let p = lookup(name)?;
    let service_user = service_user.filter(|u| *u != p.user);
    let remote = iroh_ssh::issue_invitation_command(p.endpoint, client, service_user)?;
    let mut ssh = command(&p)?;
    ssh.args(["-T", "--"]).arg(p.destination()).arg(remote);
    if !share {
        return Err(ssh.exec()).context("issue invitation over iroh");
    }
    let route = crate::invitation_cli::share_route(
        p.endpoint,
        &p.user,
        (!p.relay.is_empty()).then_some(p.relay.as_str()),
    )?;
    // Older receivers return a raw code. Keep OpenSSH's authentication and host-key
    // prompts on the user's terminal and only capture the receiver's stdout.
    let mut child = ssh
        .stdout(Stdio::piped())
        .spawn()
        .context("issue invitation over iroh")?;
    let mut output = String::new();
    let response = child
        .stdout
        .take()
        .expect("piped stdout")
        .take(1025)
        .read_to_string(&mut output);
    let status = child.wait()?;
    ensure!(status.success(), "receiver did not issue an invitation");
    response.context("read invitation from receiver")?;
    ensure!(
        output.len() <= 1024,
        "unexpected invitation response from receiver"
    );
    let code = output.parse()?;
    println!("{}", iroh_ssh::invite::SharedInvitation { route, code });
    Ok(())
}
