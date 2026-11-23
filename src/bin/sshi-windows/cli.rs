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
    command: Operation,
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

pub fn executable_command(name: &OsStr) -> Option<&'static str> {
    let stem = Path::new(name).file_stem()?.to_str()?;
    ["sshi-copy-id", "sshi-copy-iid"]
        .into_iter()
        .find(|command| stem.eq_ignore_ascii_case(command))
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
                eprintln!("Ошибка: {error:#}");
                1
            }
        },
        Err(error) => {
            let _ = error.print();
            error.exit_code()
        }
    };
    if own_window {
        eprintln!("Нажмите Enter, чтобы закрыть терминал.");
        let _ = std::io::stdin().read_line(&mut String::new());
    }
    status
}

fn execute(cli: Cli, interactive: bool) -> Result<()> {
    let store = Store::open()?;
    let (target, action) = match cli.command {
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
            let _writer = store
                .lock("window.lock", true)
                .context("Импортируйте файл через открытое окно sshi или закройте его")?;
            let count = store.restore(&file)?;
            println!("Добавлено подключений: {count}");
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
    let (user, endpoint) = iroh_ssh::profiles::destination(destination)
        .context("Укажите имя подключения или пользователь@Endpoint_ID")?;
    let mut matches = profiles
        .into_iter()
        .filter(|profile| profile.endpoint == endpoint && profile.user == user);
    if let Some(saved) = matches.next() {
        ensure!(
            matches.next().is_none(),
            "Для этого адреса есть несколько профилей. Укажите имя подключения."
        );
        return Ok(saved);
    }
    Ok(Profile {
        name: destination.to_owned(),
        endpoint,
        user: user.into(),
        relay: String::new(),
        ssh_key: String::new(),
        relay_token: Vec::new(),
    })
}
