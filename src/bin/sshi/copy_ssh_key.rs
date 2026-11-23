use std::{
    env,
    ffi::OsStr,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
};

use anyhow::{Context, Result, ensure};
use clap::{Args, Parser};
use iroh_ssh::install_ssh_key_command;

use crate::{Destination, quoted_sshi_executable, ssh_over_iroh};

#[derive(Parser)]
#[command(
    name = "sshi-copy-id",
    version,
    about = "Install an SSH public key over an iroh relay"
)]
pub struct Standalone {
    #[command(flatten)]
    pub copy: CopySshKey,
}

#[derive(Args)]
pub struct CopySshKey {
    /// Public SSH key to install. A private-key filename selects its .pub companion.
    #[arg(short = 'i', value_name = "PUBLIC_KEY")]
    identity: Option<PathBuf>,
    /// Existing SSH key to authenticate with during installation.
    #[arg(long, value_name = "PRIVATE_KEY")]
    login_key: Option<PathBuf>,
    /// Validate the key and show where it would be installed.
    #[arg(short = 'n', long)]
    dry_run: bool,
    #[arg(value_name = "USER@ENDPOINT_ID")]
    destination: Destination,
}

pub fn run(copy: CopySshKey) -> Result<()> {
    let path = select_public_key(copy.identity)?;
    let key = read_public_key(&path)?;
    if copy.dry_run {
        println!(
            "Would install {} in ~/.ssh/authorized_keys for {}",
            path.display(),
            copy.destination
        );
        return Ok(());
    }

    let exe = quoted_sshi_executable()?;
    let mut ssh = ssh_over_iroh(copy.destination.endpoint, &exe);
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
    if let Some(login_key) = copy.login_key {
        ssh.arg("-i").arg(login_key);
    }
    ssh.arg("-l")
        .arg(&copy.destination.user)
        .arg("--")
        .arg(copy.destination.endpoint.to_string())
        .arg(install_ssh_key_command());
    let status = send_input(&mut ssh, &key).context("install SSH public key over iroh")?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

fn select_public_key(identity: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = identity {
        if path.extension() == Some(OsStr::new("pub")) {
            return Ok(path);
        }
        let mut name = path.into_os_string();
        name.push(".pub");
        return Ok(name.into());
    }
    let ssh_dir =
        PathBuf::from(env::var_os("HOME").context("set HOME or choose a public key with -i")?)
            .join(".ssh");
    [
        "id_ed25519.pub",
        "id_ecdsa.pub",
        "id_rsa.pub",
        "id_ed25519_sk.pub",
        "id_ecdsa_sk.pub",
    ]
    .into_iter()
    .map(|name| ssh_dir.join(name))
    .find(|path| path.is_file())
    .context("no default SSH public key found; use -i or generate one with ssh-keygen -t ed25519")
}

fn read_public_key(path: &Path) -> Result<String> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("read SSH public key {}", path.display()))?;
    let mut lines = contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'));
    let key = lines.next().context("SSH public key file is empty")?;
    let algorithm = key.split_ascii_whitespace().next().unwrap_or_default();
    ensure!(
        lines.next().is_none()
            && ["ssh-", "ecdsa-", "sk-"]
                .iter()
                .any(|prefix| algorithm.starts_with(prefix))
            && !algorithm.contains("-cert-v01@"),
        "expected one OpenSSH public key in {}",
        path.display()
    );
    let key = format!("{key}\n");
    let status = send_input(
        Command::new("ssh-keygen")
            .args(["-l", "-f", "-"])
            .stdout(Stdio::null()),
        &key,
    )
    .context("validate SSH public key")?;
    ensure!(
        status.success(),
        "invalid SSH public key in {}",
        path.display()
    );
    Ok(key)
}

fn send_input(command: &mut Command, input: &str) -> Result<ExitStatus> {
    let mut child = command.stdin(Stdio::piped()).spawn()?;
    let written = child
        .stdin
        .take()
        .context("open command input")?
        .write_all(input.as_bytes());
    let status = child.wait()?;
    if status.success() {
        written.context("send SSH public key")?;
    }
    Ok(status)
}
