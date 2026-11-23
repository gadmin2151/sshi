mod config;
mod copy_iid;
mod copy_ssh_key;
mod invitations;
mod profiles;
mod proxy;
mod state;

use std::{
    env,
    ffi::OsString,
    fmt,
    net::SocketAddr,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::Command as Process,
    str::FromStr,
};

use anyhow::{Context, Result, bail};
use clap::{ArgAction, Parser, Subcommand};
use iroh::EndpointId;
use iroh_ssh::forwarding::{ForwardKind, ForwardSpec};
use iroh_ssh::{run_async, ssh_target};

use crate::{config::Config, state::State};

#[derive(Parser)]
#[command(
    name = "sshi",
    version,
    about = "OpenSSH over an iroh relay",
    args_conflicts_with_subcommands = true,
    arg_required_else_help = true
)]
struct Cli {
    /// SSH identity file. May be repeated.
    #[arg(short = 'i', value_name = "FILE")]
    identities: Vec<PathBuf>,
    /// Local port or Unix socket forwarding, in OpenSSH syntax.
    #[arg(short = 'L', value_name = "FORWARD")]
    local: Vec<OsString>,
    /// Remote port or Unix socket forwarding, in OpenSSH syntax.
    #[arg(short = 'R', value_name = "FORWARD")]
    remote: Vec<OsString>,
    /// Local dynamic SOCKS forwarding, in OpenSSH syntax.
    #[arg(short = 'D', value_name = "ADDRESS")]
    dynamic: Vec<OsString>,
    /// Do not execute a remote shell or command.
    #[arg(short = 'N')]
    no_command: bool,
    /// Disable remote pseudo-terminal allocation.
    #[arg(short = 'T')]
    no_tty: bool,
    /// Force remote pseudo-terminal allocation. Repeat to force with redirected stdin.
    #[arg(short = 't', action = ArgAction::Count, conflicts_with = "no_tty")]
    tty: u8,
    /// Increase OpenSSH verbosity. May be repeated.
    #[arg(short = 'v', action = ArgAction::Count)]
    verbose: u8,
    /// User account and iroh Endpoint ID of the receiving computer.
    #[arg(value_name = "USER@ENDPOINT_ID")]
    destination: Option<Destination>,
    /// Optional remote command and its arguments.
    #[arg(
        trailing_var_arg = true,
        allow_hyphen_values = true,
        value_name = "COMMAND"
    )]
    remote_command: Vec<OsString>,
    #[command(subcommand)]
    action: Option<Action>,
}

#[derive(Subcommand)]
enum Action {
    /// Print this user's persistent iroh Endpoint ID.
    Id,
    /// List successfully authenticated SSH destinations, most recent first.
    List,
    /// Manage saved connections shared by the CLI and desktop application.
    Profile {
        #[command(subcommand)]
        command: profiles::Profiles,
    },
    /// Open a saved connection and optionally execute a remote command.
    Connect {
        #[arg(allow_hyphen_values = true)]
        name: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<OsString>,
    },
    /// Forward a TCP port or start a local SOCKS proxy using a saved connection.
    Forward {
        #[arg(long, allow_hyphen_values = true)]
        profile: String,
        #[arg(long, value_enum)]
        kind: ForwardKind,
        #[arg(long, default_value = "127.0.0.1")]
        bind_host: String,
        #[arg(long)]
        bind_port: String,
        #[arg(long, requires = "target_port")]
        target_host: Option<String>,
        #[arg(long, requires = "target_host")]
        target_port: Option<String>,
    },
    /// Export all profiles and connection history to a new JSON file.
    Backup { file: PathBuf },
    /// Merge a JSON backup into this account's connections.
    Restore { file: PathBuf },
    /// Inspect effective settings or update user relay preferences.
    Config {
        #[command(subcommand)]
        command: config::Settings,
    },
    /// Issue an invitation on a saved receiver through an existing authorized SSH login.
    IssueInvite {
        #[arg(long, allow_hyphen_values = true)]
        name: String,
        #[arg(long)]
        client: EndpointId,
        #[arg(long)]
        service_user: Option<String>,
    },
    /// Add an iroh Endpoint ID to a receiver over SSH or iroh.
    #[command(alias = "compi-id")]
    CopyIid(copy_iid::CopyIid),
    /// Install a public SSH key on an authorized receiver.
    CopyId(copy_ssh_key::CopySshKey),
    /// Add a trusted iroh Endpoint ID to this receiver's configuration.
    AllowClient { id: EndpointId },
    /// Issue a one-time iroh invitation for a specific client Endpoint ID.
    Invite { id: EndpointId },
    /// Revoke a pending invitation or its granted access for an Endpoint ID.
    RevokeInvite { id: EndpointId },
    /// Accept SSH connections and share the iroh endpoint with local sshi clients.
    Serve {
        /// Authorize an incoming iroh Endpoint ID. May be repeated.
        #[arg(long, value_delimiter = ',')]
        allow_client: Vec<EndpointId>,
        /// Local sshd address. Overrides ssh_target in the configuration file.
        #[arg(long, value_parser = ssh_target)]
        target: Option<SocketAddr>,
    },
    #[command(name = "__install-key", hide = true)]
    InstallKey,
    #[command(name = "__proxy", hide = true)]
    Proxy { endpoint: EndpointId },
    #[command(name = "__broker", hide = true)]
    Broker,
    #[command(name = "__record", hide = true)]
    Record { destination: Destination },
}

#[derive(Clone)]
struct Destination {
    user: String,
    endpoint: EndpointId,
}

impl FromStr for Destination {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        let (user, endpoint) = iroh_ssh::profiles::destination(value)?;
        Ok(Self {
            user: user.to_owned(),
            endpoint,
        })
    }
}

impl fmt::Display for Destination {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(out, "{}@{}", self.user, self.endpoint)
    }
}

fn main() -> Result<()> {
    let invoked = env::args_os().next().unwrap_or_default();
    match Path::new(&invoked)
        .file_name()
        .and_then(|name| name.to_str())
    {
        Some("sshi-copy-iid" | "sshi-compi-id" | "sshi-compy-id") => {
            return copy_iid::run(copy_iid::Standalone::parse().copy);
        }
        Some("sshi-copy-id") => {
            return copy_ssh_key::run(copy_ssh_key::Standalone::parse().copy);
        }
        _ => {}
    }
    let cli = Cli::parse();
    match cli.action {
        Some(Action::Id) => {
            println!("{}", State::load()?.id()?);
            Ok(())
        }
        Some(Action::List) => State::load()?.list(),
        Some(Action::Profile { command }) => profiles::run(command),
        Some(Action::Connect { name, command }) => profiles::connect(&name, command),
        Some(Action::Forward {
            profile,
            kind,
            bind_host,
            bind_port,
            target_host,
            target_port,
        }) => {
            if kind == ForwardKind::Dynamic && target_host.is_some() {
                bail!(
                    "a SOCKS proxy takes its destination from the application; omit the target fields"
                );
            }
            let spec = ForwardSpec::new(
                kind,
                &bind_host,
                &bind_port,
                target_host.as_deref().unwrap_or_default(),
                target_port.as_deref().unwrap_or_default(),
            )?;
            let p = profiles::lookup(&profile)?;
            Err(profiles::command(&p)?
                .arg("-N")
                .args(spec.ssh_args())
                .arg("--")
                .arg(p.destination())
                .exec())
            .context("start port forwarding")
        }
        Some(Action::Backup { file }) => State::load()?.snapshot()?.write(&file),
        Some(Action::Restore { file }) => {
            let backup = iroh_ssh::profiles::Backup::read(&file)?;
            State::load()?.update_library(|library| library.merge(&backup))
        }
        Some(Action::Config { command }) => config::settings(command),
        Some(Action::IssueInvite {
            name,
            client,
            service_user,
        }) => profiles::invite(&name, client, service_user.as_deref()),
        Some(Action::InstallKey) => iroh_ssh::install_ssh_key_locally(),
        Some(Action::CopyIid(copy)) => copy_iid::run(copy),
        Some(Action::CopyId(copy)) => copy_ssh_key::run(copy),
        Some(Action::AllowClient { id }) => Config::allow_client(id),
        Some(Action::Invite { id }) => {
            println!("{}", State::load()?.invitations()?.issue(id)?);
            Ok(())
        }
        Some(Action::RevokeInvite { id }) => {
            if State::load()?.invitations()?.revoke(id)? {
                println!("Revoked invitation access for {id}");
            } else {
                println!("No invitation access was recorded for {id}");
            }
            Ok(())
        }
        Some(Action::Serve {
            allow_client,
            target,
        }) => run_async(proxy::serve(State::load()?, allow_client, target)),
        Some(Action::Proxy { endpoint }) => run_async(proxy::connect(State::load()?, endpoint)),
        Some(Action::Broker) => run_async(proxy::broker(State::load()?)),
        Some(Action::Record { destination }) => State::load()?.record(&destination.to_string()),
        None => login(&cli),
    }
}

fn login(cli: &Cli) -> Result<()> {
    let Some(destination) = &cli.destination else {
        bail!("specify the receiving computer as USER@ENDPOINT_ID");
    };
    let endpoint = destination.endpoint.to_string();
    let exe = quoted_sshi_executable()?;
    let record = ssh_shell_arg(&destination.to_string());

    let mut ssh = ssh_over_iroh(destination.endpoint, &exe);
    // OpenSSH owns authentication, host keys, signals, port forwarding and the exit status.
    ssh.args(["-o", "PermitLocalCommand=yes"])
        .arg("-o")
        .arg(format!("LocalCommand={exe} __record {record}"));

    for identity in &cli.identities {
        ssh.arg("-i").arg(identity);
    }
    let forwards = [
        ("-L", &cli.local),
        ("-R", &cli.remote),
        ("-D", &cli.dynamic),
    ];
    if forwards.iter().any(|(_, values)| !values.is_empty()) {
        ssh.args([
            "-o",
            "ExitOnForwardFailure=yes",
            "-o",
            "ClearAllForwardings=no",
        ]);
    }
    for (flag, values) in forwards {
        for value in values {
            ssh.arg(flag).arg(value);
        }
    }
    if cli.no_command {
        ssh.arg("-N");
    }
    if cli.no_tty {
        ssh.arg("-T");
    }
    for _ in 0..cli.tty {
        ssh.arg("-t");
    }
    for _ in 0..cli.verbose {
        ssh.arg("-v");
    }
    ssh.arg("-l")
        .arg(&destination.user)
        .arg("--")
        .arg(endpoint)
        .args(&cli.remote_command);

    Err(ssh.exec()).context("run OpenSSH")
}

fn quoted_sshi_executable() -> Result<String> {
    let exe = env::current_exe().context("find the sshi executable")?;
    Ok(ssh_shell_arg(
        exe.to_str()
            .context("sshi executable path must be valid UTF-8")?,
    ))
}

fn ssh_over_iroh(endpoint: EndpointId, exe: &str) -> Process {
    let mut ssh = Process::new("ssh");
    ssh.arg("-o")
        .arg(format!("ProxyCommand={exe} __proxy {endpoint}"))
        .args([
            "-o",
            "ProxyUseFdpass=no",
            "-o",
            "ControlPath=none",
            "-o",
            "CanonicalizeHostname=no",
        ])
        .arg("-o")
        .arg(format!("HostKeyAlias={endpoint}"));
    ssh
}

fn ssh_shell_arg(value: &str) -> String {
    // OpenSSH expands percent tokens before the user's shell receives the command.
    iroh_ssh::shell_arg(&value.replace('%', "%%"))
}
