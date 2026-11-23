use std::{os::unix::process::CommandExt, path::PathBuf, process::Command};

use anyhow::{Context, Result, ensure};
use clap::{Args, Parser};
use iroh::EndpointId;
use iroh_ssh::allow_client_command;

use crate::{
    Destination, ProxyMode, invitation_cli, quoted_sshi_executable, ssh_over_iroh, state::State,
};

#[derive(Parser)]
#[command(
    name = "sshi-copy-iid",
    version,
    about = "Add an iroh Endpoint ID to a receiver over SSH or iroh"
)]
pub struct Standalone {
    #[command(flatten)]
    pub copy: CopyIid,
}

#[derive(Args)]
pub struct CopyIid {
    /// SSH port for a direct SSH destination.
    #[arg(short, value_parser = clap::value_parser!(u16).range(1..))]
    port: Option<u16>,
    /// Private OpenSSH key to use for the existing SSH account.
    #[arg(short = 'i', value_name = "SSH_KEY")]
    identity: Option<PathBuf>,
    /// Public Endpoint ID to add. Defaults to this computer's ID.
    #[arg(long, value_name = "ENDPOINT_ID")]
    client_id: Option<EndpointId>,
    /// Redeem an invitation over iroh. Read the code from the terminal or stdin.
    #[arg(long, conflicts_with_all = ["port", "identity", "client_id"])]
    invite: bool,
    /// Existing SSH account and the receiver's SSH host or Endpoint ID.
    #[arg(value_name = "USER@HOST_OR_ENDPOINT_ID")]
    destination: String,
}

pub fn run(copy: CopyIid) -> Result<()> {
    if copy.invite {
        let destination: Destination = copy
            .destination
            .parse()
            .context("--invite needs a receiver in USER@ENDPOINT_ID form")?;
        return invitation_cli::redeem(
            &invitation_cli::read_input()?,
            Some(destination.endpoint),
            None,
        );
    }
    let id = match copy.client_id {
        Some(id) => id,
        None => State::load()?.id()?,
    };
    let (mut ssh, host) = match copy.destination.parse::<Destination>() {
        Ok(destination) => {
            ensure!(
                copy.port.is_none(),
                "-p is only for direct SSH destinations"
            );
            let mut ssh = ssh_over_iroh(
                destination.endpoint,
                &quoted_sshi_executable()?,
                ProxyMode::Standard,
                None,
            );
            ssh.arg("-l").arg(destination.user);
            (ssh, destination.endpoint.to_string())
        }
        Err(_) => (Command::new("ssh"), copy.destination),
    };
    ssh.args([
        "-T",
        "-a",
        "-x",
        "-o",
        "ClearAllForwardings=yes",
        "-o",
        "PermitLocalCommand=no",
        "-o",
        "RemoteCommand=none",
    ]);
    if let Some(port) = copy.port {
        ssh.arg("-p").arg(port.to_string());
    }
    if let Some(identity) = copy.identity {
        ssh.arg("-i").arg(identity);
    }
    ssh.arg("--").arg(host).arg(allow_client_command(id));
    Err(ssh.exec()).context("run OpenSSH to authorize the client Endpoint ID")
}
