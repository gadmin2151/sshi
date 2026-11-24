#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod broker;
#[cfg(windows)]
mod cli;
#[cfg(windows)]
mod file_window;
#[cfg(windows)]
mod forward_dialog;
#[cfg(windows)]
mod join_dialog;
#[cfg(windows)]
mod lang;
#[cfg(windows)]
mod store;
#[cfg(windows)]
mod terminal;
#[cfg(windows)]
mod window;

#[cfg(windows)]
const SESSION_ENV: &str = "SSHI_WINDOWS_PROFILE";

#[cfg(windows)]
fn main() {
    use crate::lang::{tr, tr_format};
    use std::{env, ptr::null_mut};

    use anyhow::{Context, Result};
    use windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONERROR;

    let mut arguments = env::args_os().collect::<Vec<_>>();
    if arguments
        .get(1)
        .is_some_and(|mode| mode == "--relay-broker")
    {
        if broker::run_child().is_err() {
            std::process::exit(1);
        }
        return;
    }
    cli::expand_short_command(&mut arguments);
    let mode = arguments.get(1).and_then(|value| value.to_str());
    let result: Result<()> = (|| match mode {
        Some("--forward-proxy") => {
            let profile = toml::from_str(
                &env::var(SESSION_ENV)
                    .context(tr("Профиль отсутствует", "Missing connection profile"))?,
            )?;
            let store = store::Store::open()?;
            iroh_ssh::run_async(terminal::proxy(&store, &profile))
        }
        Some("--files") => {
            let profile = toml::from_str(&env::var(SESSION_ENV).context("missing file profile")?)?;
            file_window::show(store::Store::open()?, profile)
        }
        Some("--id") => {
            println!("{}", store::Store::open()?.identity()?.public());
            Ok(())
        }
        Some(
            "--terminal" | "--terminal-io" | "--enroll" | "--enroll-io" | "--copy-ssh-key"
            | "--copy-ssh-key-io" | "--issue-invite" | "--issue-invite-io" | "--forward"
            | "--forward-io",
        ) => {
            let interactive = matches!(
                mode,
                Some("--terminal" | "--enroll" | "--copy-ssh-key" | "--issue-invite" | "--forward")
            );
            let enrollment = matches!(mode, Some("--enroll" | "--enroll-io"));
            let install_key = matches!(mode, Some("--copy-ssh-key" | "--copy-ssh-key-io"));
            let issue_invite = matches!(mode, Some("--issue-invite" | "--issue-invite-io"));
            let profile: store::Profile = toml::from_str(
                &env::var(SESSION_ENV)
                    .context(tr("Профиль отсутствует", "Missing connection profile"))?,
            )?;
            let store = store::Store::open()?;
            if interactive {
                if issue_invite {
                    terminal::open_window(&tr_format!(
                        "Создать приглашение — {}",
                        "Create invitation — {}",
                        profile.name
                    ))?;
                } else {
                    terminal::open_window(&profile.name)?;
                }
            }
            let result = {
                if matches!(mode, Some("--forward" | "--forward-io")) {
                    terminal::forward(&profile, &arguments[2..])
                } else if enrollment {
                    iroh_ssh::run_async(terminal::enroll(store, profile, interactive))
                } else if issue_invite {
                    if let [guest, account] = &arguments[2..] {
                        let guest = guest.to_str().context("Invalid guest ID")?.parse()?;
                        let account = account.to_str().context("Invalid receiver account")?;
                        iroh_ssh::run_async(terminal::issue_to(
                            store,
                            profile,
                            interactive,
                            guest,
                            account,
                        ))
                    } else {
                        iroh_ssh::run_async(terminal::issue_invite(store, profile, interactive))
                    }
                } else {
                    let action = if install_key {
                        terminal::RelayAction::CopySshKey
                    } else {
                        terminal::RelayAction::Shell
                    };
                    iroh_ssh::run_async(terminal::connect(store, profile, interactive, action))
                }
            };
            if interactive && (enrollment || install_key || result.is_err()) {
                if let Err(error) = &result {
                    eprintln!(
                        "{}",
                        tr_format!("\nОшибка: {error:#}", "\nError: {error:#}")
                    );
                }
                eprintln!(
                    "{}",
                    tr(
                        "Нажмите Enter, чтобы закрыть терминал.",
                        "Press Enter to close the terminal."
                    )
                );
                let _ = std::io::stdin().read_line(&mut String::new());
            }
            result
        }
        None if arguments.len() < 2 => window::show(store::Store::open()?),
        _ => std::process::exit(cli::run(&arguments)),
    })();
    if let Err(error) = result {
        match mode {
            Some("--terminal" | "--enroll" | "--copy-ssh-key" | "--issue-invite" | "--forward") => {
                // Interactive session errors are printed in their terminal.
                if unsafe { windows_sys::Win32::System::Console::GetConsoleWindow() }.is_null() {
                    window::message_box(null_mut(), &format!("{error:#}"), MB_ICONERROR);
                }
            }
            Some(
                "--id" | "--terminal-io" | "--enroll-io" | "--copy-ssh-key-io"
                | "--issue-invite-io" | "--forward-proxy" | "--forward-io",
            ) => {
                eprintln!("{error:#}");
            }
            _ => {
                window::message_box(null_mut(), &format!("{error:#}"), MB_ICONERROR);
            }
        }
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Build sshi-windows for Windows.");
}
