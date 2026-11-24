use crate::{config::Config, profiles, state::State};
use anyhow::{Context, Result, bail, ensure};
use clap::Subcommand;
use iroh_ssh::{profiles::Profile, shell_arg};
use std::{
    fs::{self, File, OpenOptions},
    io::{ErrorKind, Read, Seek, SeekFrom},
    os::unix::{
        fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt},
        net::UnixStream,
    },
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

#[derive(Subcommand)]
pub enum Action {
    /// Print the local socket path.
    Path { profile: String },
    /// Print a shell command that uses only this socket.
    Command { profile: String },
    /// Authenticate through Iroh and keep a master running in the background.
    Start {
        #[arg(long, default_value = "700m")]
        persist: String,
        /// Use the configured SSH key without requesting interactive input.
        #[arg(long)]
        batch: bool,
        profile: String,
    },
    /// Ask the local SSH master whether it is running.
    Check { profile: String },
    /// Stop the master and close its SSH sessions.
    Close { profile: String },
}

fn socket_path(directory: &Path, profile: &Profile, identity_path: &Path) -> PathBuf {
    let mut hash = blake3::Hasher::new();
    for part in [
        identity_path.as_os_str().as_encoded_bytes(),
        profile.relay.as_bytes(),
        profile.destination().as_bytes(),
    ] {
        hash.update(part);
        hash.update(&[0]);
    }
    let digest = hash.finalize();
    directory.join(format!(
        "{}-{}.sock",
        &profile.endpoint.to_string()[..12],
        &digest.to_hex()[..24]
    ))
}

fn verify_directory(directory: &Path, uid: u32, create: bool) -> Result<()> {
    if create {
        match fs::DirBuilder::new().mode(0o700).create(directory) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e).context("create private SSH socket directory"),
        }
    }
    let meta = fs::symlink_metadata(directory).context("open private SSH socket directory")?;
    ensure!(
        meta.is_dir() && meta.uid() == uid && meta.mode() & 0o077 == 0,
        "{} must be a private directory owned by the current user (mode 0700)",
        directory.display()
    );
    Ok(())
}

fn lock(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path.with_extension("lock"))?;
    file.lock().context("lock SSH socket operation")?;
    Ok(file)
}

fn socket_exists(path: &Path, uid: u32) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            ensure!(
                meta.file_type().is_socket() && meta.uid() == uid,
                "expected an owned Unix socket at {}",
                path.display()
            );
            Ok(true)
        }
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

fn control(path: &Path, dest: &str, op: &str) -> Result<Output> {
    Command::new("ssh")
        .arg("-S")
        .arg(path)
        .args(["-o", "ProxyCommand=false", "-O", op, "--", dest])
        .output()
        .context("run OpenSSH control command")
}

fn resolve(profile: &Profile) -> Result<Profile> {
    let mut profile = profile.clone();
    let relay = if profile.relay.is_empty() {
        Config::load()?.default_relay()?.unwrap_or_default()
    } else {
        profile.relay.clone()
    };
    if !relay.is_empty() {
        profile.relay = iroh_ssh::relay_url(&relay)?.to_string();
    }
    Ok(profile)
}

fn managed_path(profile: &Profile) -> Result<(PathBuf, PathBuf, u32)> {
    // geteuid takes no arguments and has no failure mode.
    let uid = unsafe { libc::geteuid() };
    let directory = PathBuf::from(format!("/tmp/sshi-{uid}"));
    let path = socket_path(&directory, profile, &State::load()?.key());
    Ok((directory, path, uid))
}

pub fn existing(profile: &Profile) -> Result<Option<PathBuf>> {
    let profile = resolve(profile)?;
    let (directory, path, uid) = managed_path(&profile)?;
    match verify_directory(&directory, uid, false) {
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        result => {
            result?;
            if socket_exists(&path, uid)?
                && control(&path, &profile.destination(), "check")?
                    .status
                    .success()
            {
                Ok(Some(path))
            } else {
                Ok(None)
            }
        }
    }
}

pub fn run(action: Action) -> Result<()> {
    let name = match &action {
        Action::Path { profile }
        | Action::Command { profile }
        | Action::Start { profile, .. }
        | Action::Check { profile }
        | Action::Close { profile } => profile,
    };
    let profile = resolve(&profiles::lookup(name)?)?;
    let dest = profile.destination();
    let (directory, path, uid) = managed_path(&profile)?;
    match action {
        Action::Path { .. } => println!("{}", path.display()),
        Action::Command { .. } => println!(
            "ssh -S {} -o ProxyCommand=false -o BatchMode=yes -- {}",
            shell_arg(&path.to_string_lossy()),
            shell_arg(&dest)
        ),
        Action::Start { persist, batch, .. } => {
            verify_directory(&directory, uid, true)?;
            let _lock = lock(&path)?;
            if socket_exists(&path, uid)? {
                if control(&path, &dest, "check")?.status.success() {
                    println!("{}", path.display());
                    return Ok(());
                }
                // A master can disappear between checks. Remove only a stale socket.
                match UnixStream::connect(&path) {
                    Err(e) if e.kind() == ErrorKind::NotFound => {}
                    Err(e) if e.kind() == ErrorKind::ConnectionRefused => fs::remove_file(&path)?,
                    _ => bail!(
                        "existing socket did not answer OpenSSH at {}",
                        path.display()
                    ),
                }
            }
            let mut process = profiles::command(&profile)?;
            let diagnostics = if batch {
                // ProxyCommand can inherit stderr before OpenSSH forks. Use a
                // temporary file so its lifetime does not keep GUI pipes open.
                let file = tempfile::tempfile().context("store SSH startup diagnostics")?;
                process
                    .arg("--batch-login")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(file.try_clone()?);
                Some(file)
            } else {
                None
            };
            let status = process
                .args(["-f", "-M", "-N", "-S"])
                .arg(&path)
                .args(["-o", &format!("ControlPersist={persist}"), "--", &dest])
                .status()
                .context("start SSH master through Iroh")?;
            if !status.success() {
                if let Some(mut file) = diagnostics {
                    let offset = file.metadata()?.len().saturating_sub(4096);
                    file.seek(SeekFrom::Start(offset))?;
                    let mut tail = Vec::new();
                    file.take(4096).read_to_end(&mut tail)?;
                    let detail = String::from_utf8_lossy(&tail);
                    if !detail.trim().is_empty() {
                        bail!("{}", detail.trim());
                    }
                }
                bail!("SSH master exited with {status}");
            }
            let check = control(&path, &dest, "check")?;
            ensure!(
                check.status.success(),
                "{}",
                String::from_utf8_lossy(&check.stderr).trim()
            );
            println!("{}", path.display());
        }
        Action::Check { .. } | Action::Close { .. } => {
            if let Err(error) = verify_directory(&directory, uid, false) {
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == ErrorKind::NotFound)
                {
                    bail!("No active SSH control socket; run sshi socket start PROFILE first");
                }
                return Err(error);
            }
            let closing = matches!(action, Action::Close { .. });
            let _lock = if closing { Some(lock(&path)?) } else { None };
            ensure!(
                socket_exists(&path, uid)?,
                "No active SSH control socket at {}; run sshi socket start PROFILE first",
                path.display()
            );
            let result = control(&path, &dest, if closing { "exit" } else { "check" })?;
            let message = String::from_utf8_lossy(&result.stderr);
            if !result.status.success() {
                if !socket_exists(&path, uid)?
                    || matches!(UnixStream::connect(&path), Err(e) if matches!(e.kind(), ErrorKind::ConnectionRefused | ErrorKind::NotFound))
                {
                    bail!(
                        "No active SSH control socket at {}; run sshi socket start PROFILE first",
                        path.display()
                    );
                }
                bail!("{}", message.trim());
            }
            println!("{}", message.trim());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    #[test]
    fn profile_socket_tracks_the_route_and_account_across_renames() {
        let dir = Path::new("/tmp/sshi-1000");
        let key = Path::new("/example/state/endpoint.key");
        let mut profile = Profile {
            name: "Office".into(),
            user: "operator".into(),
            endpoint: iroh::SecretKey::from_bytes(&[2; 32]).public(),
            relay: "https://relay.example/".into(),
            notes: String::new(),
        };
        let path = socket_path(dir, &profile, key);
        profile.name = "Workshop".into();
        assert_eq!(path, socket_path(dir, &profile, key));
        assert_ne!(
            path,
            socket_path(dir, &profile, Path::new("/another/state/endpoint.key"))
        );
        profile.user = "pi".into();
        assert_ne!(path, socket_path(dir, &profile, key));
        profile.user = "operator".into();
        profile.relay = "https://other.example/".into();
        assert_ne!(path, socket_path(dir, &profile, key));
    }

    #[test]
    fn private_directory_rejects_public_paths_and_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        let uid = fs::metadata(temp.path()).unwrap().uid();
        let private = temp.path().join("private");
        verify_directory(&private, uid, true).unwrap();
        let alias = temp.path().join("alias");
        symlink(&private, &alias).unwrap();
        assert!(verify_directory(&alias, uid, true).is_err());
        fs::set_permissions(&private, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(verify_directory(&private, uid, true).is_err());
    }
}
