use crate::lang::{tr, tr_format};
use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    ptr::null_mut,
};

use anyhow::{Context, Result, ensure};
use clap::{Args, Parser, Subcommand};
use iroh::EndpointId;
use iroh_ssh::run_async;
use windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONERROR;

use crate::{
    store::{Profile, Store},
    terminal::{self, RelayAction},
    window,
};

#[derive(Parser)]
#[command(name = "sshi-portable-x64", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Copy files over SSH and Iroh using SFTP.
    #[command(long_flag = "cp")]
    Cp(iroh_ssh::file_transfer::CopyArgs),
    /// Check the latest stable GitHub release and show the package for this system.
    Update {
        #[arg(long)]
        json: bool,
    },
    #[command(flatten)]
    Local(Operation),
}

#[derive(Subcommand)]
enum Operation {
    /// Print this computer's public Iroh Endpoint ID.
    Id,
    /// Show saved profiles and connection history.
    List,
    /// Open an interactive SSH session by profile name or user@EndpointID.
    Connect(Target),
    /// Bridge stdin/stdout to a receiver for OpenSSH ProxyCommand.
    Proxy {
        #[arg(value_name = "PROFILE_OR_USER@ENDPOINT_ID")]
        destination: String,
    },
    /// Export portable connections and history to a new JSON file.
    Backup { file: PathBuf },
    /// Merge a portable JSON backup into saved connections.
    Restore { file: PathBuf },
    /// Install a public SSH key over iroh.
    #[command(name = "sshi-copy-id", visible_alias = "copy-id")]
    CopyId(Target),
    /// Add an iroh Endpoint ID to a receiver's allowlist over iroh.
    #[command(name = "sshi-copy-iid", visible_alias = "copy-iid")]
    CopyIid {
        #[command(flatten)]
        target: Target,
        /// Public Endpoint ID to add. Defaults to this computer's ID.
        #[arg(long, value_name = "ENDPOINT_ID")]
        client_id: Option<EndpointId>,
        /// Redeem an invitation over iroh. Read the code from the terminal or stdin.
        #[arg(long, conflicts_with_all = ["identity", "client_id"])]
        invite: bool,
    },
}

#[derive(Args)]
struct Target {
    /// OpenSSH private key file to use.
    #[arg(short, value_name = "SSH_KEY")]
    identity: Option<String>,
    /// SSH account and the receiver's iroh Endpoint ID.
    #[arg(value_name = "USER@ENDPOINT_ID")]
    destination: String,
}

fn executable_command(name: &OsStr) -> Option<&'static str> {
    let stem = Path::new(name).file_stem()?.to_str()?;
    ["sshi-copy-id", "sshi-copy-iid"]
        .into_iter()
        .find(|command| stem.eq_ignore_ascii_case(command))
}

pub fn expand_short_command(arguments: &mut Vec<OsString>) {
    if let Some(command) = arguments.first().and_then(|name| executable_command(name)) {
        arguments.insert(1, command.into());
    } else if arguments
        .get(1)
        .and_then(|arg| arg.to_str())
        .is_some_and(|value| iroh_ssh::profiles::destination(value).is_ok())
    {
        arguments.insert(1, "connect".into());
    }
}

pub fn run(arguments: &[OsString]) -> i32 {
    let own_window = match terminal::open_command_window() {
        Ok(own_window) => own_window,
        Err(error) => {
            window::message_box(null_mut(), &format!("{error:#}"), MB_ICONERROR);
            return 1;
        }
    };
    let status = match Cli::try_parse_from(arguments) {
        Ok(cli) => match execute(cli, terminal::has_console_input()) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("{}", tr_format!("Ошибка: {error:#}", "Error: {error:#}"));
                1
            }
        },
        Err(error) => {
            let _ = error.print();
            error.exit_code()
        }
    };
    if own_window {
        eprintln!(
            "{}",
            tr(
                "Нажмите Enter, чтобы закрыть терминал.",
                "Press Enter to close the terminal."
            )
        );
        let _ = std::io::stdin().read_line(&mut String::new());
    }
    status
}

fn execute(cli: Cli, interactive: bool) -> Result<()> {
    let operation = match cli.command {
        Command::Cp(args) => {
            let store = Store::open()?;
            let (name, transfer) = args.transfer()?;
            let target = Target {
                destination: name.into(),
                identity: args
                    .identity
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned()),
            };
            let profile = profile(&store, target)?;
            return run_async(async {
                let session = terminal::open_files(
                    &store,
                    &profile,
                    terminal::LoginInput::Console(interactive),
                )
                .await?;
                let count = iroh_ssh::file_transfer::copy(
                    &session.sftp,
                    transfer,
                    args.recursive,
                    iroh_ssh::file_transfer::print_progress,
                )
                .await?;
                session.sftp.close().await?;
                eprintln!(
                    "{}",
                    tr_format!("Скопировано файлов — {count}", "Files copied — {count}")
                );
                Ok(())
            });
        }
        Command::Update { json } => return run_async(iroh_ssh::update::print_check(json)),
        Command::Local(operation) => operation,
    };
    let store = Store::open()?;
    let (target, action) = match operation {
        Operation::Id => {
            println!("{}", store.identity()?.public());
            return Ok(());
        }
        Operation::List => {
            for profile in store.profiles()? {
                println!("{}\t{}@{}", profile.name, profile.user, profile.endpoint);
            }
            for destination in store.history()? {
                println!("{destination}");
            }
            return Ok(());
        }
        Operation::Connect(target) => (target, RelayAction::Shell),
        Operation::Proxy { destination } => {
            let profile = profile(
                &store,
                Target {
                    identity: None,
                    destination,
                },
            )?;
            return run_async(terminal::proxy(&store, &profile));
        }
        Operation::Backup { file } => return store.backup(&file),
        Operation::Restore { file } => {
            let _writer = store.lock("window.lock", true).context(tr(
                "Импортируйте файл через открытое окно sshi или закройте его",
                "Import through the open SSHI window or close it first",
            ))?;
            let count = store.restore(&file)?;
            println!(
                "{}",
                tr_format!(
                    "Добавлено подключений: {count}",
                    "Connections added: {count}"
                )
            );
            return Ok(());
        }
        Operation::CopyId(target) => (target, RelayAction::CopySshKey),
        Operation::CopyIid {
            target,
            client_id,
            invite,
        } => {
            if invite {
                let profile = profile(&store, target)?;
                return run_async(terminal::enroll(store, profile, interactive));
            }
            let id = match client_id {
                Some(id) => id,
                None => store.identity()?.public(),
            };
            (target, RelayAction::CopyEndpointId(id))
        }
    };
    let profile = profile(&store, target)?;
    run_async(terminal::connect(store, profile, interactive, action))
}

fn profile(store: &Store, target: Target) -> Result<Profile> {
    let profiles = store.profiles()?;
    let named = profiles
        .iter()
        .find(|profile| profile.name.to_lowercase() == target.destination.to_lowercase());
    let mut profile = if let Some(saved) = named {
        saved.clone()
    } else {
        from_destination(&target.destination, profiles)?
    };
    if let Some(identity) = target.identity {
        profile.ssh_key = identity;
    }
    Ok(profile)
}

fn from_destination(destination: &str, profiles: Vec<Profile>) -> Result<Profile> {
    let (user, endpoint) = iroh_ssh::profiles::destination(destination).context(tr(
        "Укажите имя подключения или пользователь@Endpoint_ID",
        "Enter a connection name or user@Endpoint_ID",
    ))?;
    let mut matches = profiles
        .into_iter()
        .filter(|profile| profile.endpoint == endpoint && profile.user == user);
    if let Some(saved) = matches.next() {
        ensure!(
            matches.next().is_none(),
            tr(
                "Для этого адреса есть несколько профилей. Укажите имя подключения.",
                "Multiple profiles use this address. Enter the connection name."
            )
        );
        return Ok(saved);
    }
    Ok(Profile {
        name: destination.to_owned(),
        endpoint,
        user: user.into(),
        relay: String::new(),
        notes: String::new(),
        ssh_key: String::new(),
        relay_token: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_only_valid_bare_destinations_and_keeps_executable_aliases() {
        let address = format!("pi@{}", iroh::SecretKey::from_bytes(&[7; 32]).public());
        for (args, expected) in [
            (
                vec!["sshi.exe", &address, "-i", "file"],
                vec!["sshi.exe", "connect", &address, "-i", "file"],
            ),
            (
                vec!["sshi-copy-id.exe", &address],
                vec!["sshi-copy-id.exe", "sshi-copy-id", &address],
            ),
            (vec!["sshi.exe", "pi@bad"], vec!["sshi.exe", "pi@bad"]),
            (
                vec!["sshi.exe", "connect", "studio"],
                vec!["sshi.exe", "connect", "studio"],
            ),
            (vec!["sshi.exe", "--help"], vec!["sshi.exe", "--help"]),
            (vec!["sshi.exe"], vec!["sshi.exe"]),
        ] {
            let mut actual: Vec<OsString> = args.into_iter().map(Into::into).collect();
            expand_short_command(&mut actual);
            assert_eq!(actual, expected);
        }
    }
}
