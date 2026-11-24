mod config;
mod control_socket;
mod copy_iid;
mod copy_ssh_key;
mod files;
mod invitation_cli;
mod invitations;
mod login_invite;
mod profiles;
mod proxy;
mod state;

use std::{
    env,
    ffi::OsString,
    fmt,
    io::{self, IsTerminal},
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
    /// Create an OpenSSH master connection on the socket selected with -S.
    #[arg(
        short = 'M',
        requires = "control_path",
        conflicts_with = "control_command"
    )]
    master: bool,
    /// Local OpenSSH control socket for this connection.
    #[arg(short = 'S', value_name = "SOCKET")]
    control_path: Option<PathBuf>,
    /// Check or stop an existing OpenSSH master.
    #[arg(short = 'O', value_name = "OPERATION", requires = "control_path", value_parser = ["check", "exit"])]
    control_command: Option<String>,
    /// Set the idle lifetime of an OpenSSH master, e.g. -o ControlPersist=700m.
    #[arg(short = 'o', value_name = "ControlPersist=TIME", requires = "control_path", value_parser = control_persist)]
    persist: Option<String>,
    /// Background OpenSSH after authentication.
    #[arg(short = 'f', conflicts_with = "control_command")]
    background: bool,
    #[arg(long, hide = true, requires = "master")]
    batch_login: bool,
    /// Disable remote pseudo-terminal allocation.
    #[arg(short = 'T')]
    no_tty: bool,
    /// Force remote pseudo-terminal allocation. Repeat to force with redirected stdin.
    #[arg(short = 't', action = ArgAction::Count, conflicts_with = "no_tty")]
    tty: u8,
    /// Increase OpenSSH verbosity. May be repeated.
    #[arg(short = 'v', action = ArgAction::Count)]
    verbose: u8,
    /// Saved connection name or user account and iroh Endpoint ID.
    #[arg(value_name = "USER@ENDPOINT_ID|PROFILE")]
    destination: Option<String>,
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
    /// Copy files over SSH and Iroh using SFTP.
    #[command(long_flag = "cp")]
    Cp(iroh_ssh::file_transfer::CopyArgs),
    #[command(name = "__files", hide = true)]
    FileBrowser { profile: String },
    /// Manage a local OpenSSH control socket for a saved Iroh connection.
    Socket {
        #[command(subcommand)]
        command: control_socket::Action,
    },
    /// Check the latest stable GitHub release and show the package for this system.
    Update {
        #[arg(long)]
        json: bool,
    },
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
        /// Print a shareable link containing the receiver settings.
        #[arg(long)]
        share: bool,
    },
    /// Add an iroh Endpoint ID to a receiver over SSH or iroh.
    #[command(alias = "compi-id")]
    CopyIid(copy_iid::CopyIid),
    /// Install a public SSH key on an authorized receiver.
    CopyId(copy_ssh_key::CopySshKey),
    /// Add a trusted iroh Endpoint ID to this receiver's configuration.
    AllowClient { id: EndpointId },
    /// Issue a one-time iroh invitation for a specific client Endpoint ID.
    Invite {
        id: EndpointId,
        /// Include the receiver's SSH user, Endpoint ID and relay in a shareable link.
        #[arg(long, requires = "user")]
        share: bool,
        #[arg(long, requires = "share")]
        user: Option<String>,
        #[arg(long, requires = "share")]
        relay: Option<String>,
    },
    /// Inspect or redeem an invitation supplied on stdin.
    Invitation {
        #[command(subcommand)]
        command: invitation_cli::Action,
    },
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
    Proxy {
        endpoint: EndpointId,
        #[arg(long)]
        invite_user: Option<String>,
        #[arg(long, conflicts_with = "invite_user")]
        background_master: bool,
    },
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
        Some(Action::Cp(args)) => run_async(files::copy(args)),
        Some(Action::FileBrowser { profile }) => run_async(files::browser(&profile)),
        Some(Action::Socket { command }) => control_socket::run(command),
        Some(Action::Update { json }) => run_async(iroh_ssh::update::print_check(json)),
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
            share,
        }) => profiles::invite(&name, client, service_user.as_deref(), share),
        Some(Action::InstallKey) => iroh_ssh::install_ssh_key_locally(),
        Some(Action::CopyIid(copy)) => copy_iid::run(copy),
        Some(Action::CopyId(copy)) => copy_ssh_key::run(copy),
        Some(Action::AllowClient { id }) => Config::allow_client(id),
        Some(Action::Invite {
            id, user, relay, ..
        }) => invitation_cli::issue(id, user.as_deref(), relay.as_deref()),
        Some(Action::Invitation { command }) => invitation_cli::run(command),
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
        Some(Action::Proxy {
            endpoint,
            invite_user,
            background_master,
        }) => {
            if background_master {
                // OpenSSH forks its master after authentication. Its proxy was forked
                // earlier and otherwise still receives hangups from the login terminal.
                // setsid preserves OpenSSH's pipes and its ability to signal us directly.
                if unsafe { libc::setsid() } == -1 {
                    return Err(io::Error::last_os_error())
                        .context("detach SSH master proxy from its terminal");
                }
            }
            run_async(proxy::connect(
                State::load()?,
                endpoint,
                invite_user.as_deref(),
            ))
        }
        Some(Action::Broker) => run_async(proxy::broker(State::load()?)),
        Some(Action::Record { destination }) => State::load()?.record(&destination.to_string()),
        None => login(&cli),
    }
}

fn login(cli: &Cli) -> Result<()> {
    Err(login_command(cli)?.exec()).context("run OpenSSH")
}

fn login_command(cli: &Cli) -> Result<Process> {
    let target = cli
        .destination
        .as_deref()
        .context("specify USER@ENDPOINT_ID or a saved connection name")?;
    let (destination, profile) = if let Ok(destination) = target.parse::<Destination>() {
        (destination, None)
    } else {
        let profile = profiles::lookup(target)?;
        (
            Destination {
                user: profile.user.clone(),
                endpoint: profile.endpoint,
            },
            Some(profile),
        )
    };
    let endpoint = destination.endpoint.to_string();
    let exe = quoted_sshi_executable()?;
    let record = ssh_shell_arg(&destination.to_string());

    let proxy_mode = if cli.master
        && (cli.background
            || (cli.no_command
                && cli
                    .persist
                    .as_ref()
                    .is_some_and(|p| !p.eq_ignore_ascii_case("no"))))
    {
        ProxyMode::BackgroundMaster
    } else if cli.interactive_shell()
        && io::stdin().is_terminal()
        && io::stdout().is_terminal()
        && io::stderr().is_terminal()
    {
        ProxyMode::Invite(&destination.user)
    } else {
        ProxyMode::Standard
    };
    let mut ssh = ssh_over_iroh(
        destination.endpoint,
        &exe,
        proxy_mode,
        cli.control_path.as_deref(),
    );
    if let Some(profile) = profile {
        profiles::set_relay(&mut ssh, &profile);
    }
    if cli.batch_login {
        ssh.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=25"]);
    }
    if cli.master {
        ssh.arg("-M");
    }
    if let Some(command) = &cli.control_command {
        ssh.args(["-O", command]);
    }
    if let Some(value) = &cli.persist {
        ssh.arg("-o").arg(format!("ControlPersist={value}"));
    }
    if cli.background {
        ssh.arg("-f");
    }
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

    Ok(ssh)
}

fn quoted_sshi_executable() -> Result<String> {
    let exe = env::current_exe().context("find the sshi executable")?;
    Ok(ssh_shell_arg(
        exe.to_str()
            .context("sshi executable path must be valid UTF-8")?,
    ))
}

impl Cli {
    fn interactive_shell(&self) -> bool {
        self.remote_command.is_empty()
            && !self.no_tty
            && !self.no_command
            && !self.background
            && self.control_command.is_none()
            && self.local.is_empty()
            && self.remote.is_empty()
            && self.dynamic.is_empty()
    }
}

fn control_persist(option: &str) -> Result<String, String> {
    let (key, value) = option
        .split_once('=')
        .or_else(|| option.split_once(char::is_whitespace))
        .ok_or("expected -o ControlPersist=TIME")?;
    if !key.trim().eq_ignore_ascii_case("ControlPersist") {
        return Err("the supported -o option is ControlPersist=TIME".into());
    }
    let value = value.trim();
    if !(value.eq_ignore_ascii_case("yes")
        || value.eq_ignore_ascii_case("no")
        || value.starts_with(|c: char| c.is_ascii_digit())
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || b"smhdw".contains(&b.to_ascii_lowercase())))
    {
        return Err("expected yes, no or an OpenSSH time such as 700m".into());
    }
    Ok(value.to_owned())
}

enum ProxyMode<'a> {
    Standard,
    Invite(&'a str),
    BackgroundMaster,
}

fn ssh_over_iroh(
    endpoint: EndpointId,
    exe: &str,
    mode: ProxyMode<'_>,
    control_path: Option<&Path>,
) -> Process {
    let mut ssh = Process::new("ssh");
    let mut proxy = format!("ProxyCommand={exe} __proxy {endpoint}");
    match mode {
        ProxyMode::Standard => {}
        ProxyMode::BackgroundMaster => proxy.push_str(" --background-master"),
        ProxyMode::Invite(user) => {
            proxy.push_str(" --invite-user ");
            proxy.push_str(&ssh_shell_arg(user));
        }
    }
    if let Some(path) = control_path {
        ssh.arg("-S").arg(path);
    } else {
        ssh.args(["-o", "ControlPath=none"]);
    }
    ssh.arg("-o")
        .arg(proxy)
        .args(["-o", "ProxyUseFdpass=no", "-o", "CanonicalizeHostname=no"])
        .arg("-o")
        .arg(format!("HostKeyAlias={endpoint}"));
    ssh
}

fn ssh_shell_arg(value: &str) -> String {
    // OpenSSH expands percent tokens before the user's shell receives the command.
    iroh_ssh::shell_arg(&value.replace('%', "%%"))
}

#[cfg(test)]
mod control_tests {
    use super::*;
    use iroh::SecretKey;

    #[test]
    fn master_uses_explicit_socket_with_the_same_endpoint_and_proxy() -> Result<()> {
        let endpoint = SecretKey::generate().public();
        let target = format!("pi@{endpoint}");
        let cli = Cli::try_parse_from([
            "sshi",
            "-fMN",
            "-S",
            "/tmp/desk socket",
            "-o",
            "ControlPersist=700m",
            &target,
        ])?;
        assert!(!cli.interactive_shell());
        let ssh = login_command(&cli)?;
        let args: Vec<_> = ssh.get_args().map(|s| s.to_str().unwrap()).collect();
        assert!(args.windows(2).any(|a| a == ["-S", "/tmp/desk socket"]));
        assert!(args.windows(2).any(|a| a == ["-o", "ControlPersist=700m"]));
        for flag in ["-M", "-f", "-N"] {
            assert!(args.contains(&flag));
        }
        assert!(!args.contains(&"ControlPath=none"));
        assert!(args.iter().any(|s| s.starts_with("ProxyCommand=")
            && s.ends_with(&format!("__proxy {endpoint} --background-master"))));
        assert!(args.contains(&format!("HostKeyAlias={endpoint}").as_str()));

        let cli = Cli::try_parse_from(["sshi", &target])?;
        assert!(
            !login_command(&cli)?
                .get_args()
                .any(|a| a.to_string_lossy().contains("--background-master"))
        );
        assert!(
            login_command(&cli)?
                .get_args()
                .any(|a| a == "ControlPath=none")
        );
        Ok(())
    }

    #[test]
    fn socket_operations_keep_options_separate_from_remote_commands() -> Result<()> {
        let endpoint = SecretKey::generate().public();
        let target = format!("pi@{endpoint}");
        for operation in ["check", "exit"] {
            let cli = Cli::try_parse_from(["sshi", "-S", "test.sock", "-O", operation, &target])?;
            assert!(!cli.interactive_shell());
            let ssh = login_command(&cli)?;
            let args: Vec<_> = ssh.get_args().map(|s| s.to_str().unwrap()).collect();
            assert!(args.windows(2).any(|a| a == ["-O", operation]));
        }
        let cli = Cli::try_parse_from(["sshi", "-S", "test.sock", &target, "echo", "-M"])?;
        let ssh = login_command(&cli)?;
        let args: Vec<_> = ssh.get_args().map(|s| s.to_str().unwrap()).collect();
        assert!(args.ends_with(&[&endpoint.to_string(), "echo", "-M"]));
        assert!(Cli::try_parse_from(["sshi", "-M", &target]).is_err());
        assert!(Cli::try_parse_from(["sshi", "-O", "check", &target]).is_err());
        assert!(
            Cli::try_parse_from([
                "sshi",
                "-S",
                "test.sock",
                "-o",
                "ProxyCommand=false",
                &target
            ])
            .is_err()
        );
        Ok(())
    }
}
