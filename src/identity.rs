use std::{
    fs::{File, TryLockError},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use iroh::{EndpointId, SecretKey};
use tempfile::NamedTempFile;

#[derive(Debug)]
pub struct KeyInUse(PathBuf);

impl std::fmt::Display for KeyInUse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "iroh key {} is already in use", self.0.display())
    }
}

impl std::error::Error for KeyInUse {}

pub fn generate_key(path: &Path) -> Result<EndpointId> {
    let key = SecretKey::generate();
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut file = NamedTempFile::new_in(parent)
        .with_context(|| format!("create key in {}", parent.display()))?;
    file.write_all(&key.to_bytes()).context("write iroh key")?;
    file.as_file().sync_all().context("sync iroh key")?;
    file.persist_noclobber(path)
        .map_err(|error| error.error)
        .with_context(|| format!("save new key to {}", path.display()))?;
    Ok(key.public())
}

pub fn key_id(path: &Path) -> Result<EndpointId> {
    Ok(read_secret(&mut open_key(path)?)?.public())
}

pub(crate) fn lock_key(path: &Path) -> Result<(SecretKey, File)> {
    let mut file = open_key(path)?;
    // A new registration for the same Endpoint ID displaces the relay's active route.
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Err(KeyInUse(path.to_owned()).into()),
        Err(TryLockError::Error(error)) => {
            return Err(error).with_context(|| format!("lock iroh key {}", path.display()));
        }
    }
    Ok((read_secret(&mut file)?, file))
}

fn open_key(path: &Path) -> Result<File> {
    let file = File::open(path).with_context(|| format!("open iroh key {}", path.display()))?;
    let metadata = file.metadata().context("read iroh key metadata")?;
    ensure!(metadata.is_file(), "iroh key must be a regular file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            metadata.permissions().mode() & 0o077 == 0,
            "iroh key {} is accessible to other users; set its mode to 600",
            path.display()
        );
    }
    ensure!(
        metadata.len() == 32,
        "iroh key must contain exactly 32 bytes"
    );
    Ok(file)
}

fn read_secret(file: &mut File) -> Result<SecretKey> {
    let mut bytes = [0; 32];
    file.read_exact(&mut bytes).context("read iroh key")?;
    Ok(SecretKey::from_bytes(&bytes))
}
