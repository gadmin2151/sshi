//! Portable connection metadata. Credentials and machine-specific paths are kept separately.
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{Read, Write},
    path::Path,
};

use anyhow::{Context, Result, ensure};
use iroh::EndpointId;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

const VERSION: u32 = 1;
const MAX_BYTES: u64 = 16 * 1024 * 1024;

pub fn valid_user(user: &str) -> bool {
    !user.is_empty()
        && user.len() <= 256
        && !user.starts_with('-')
        && !user
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace() || ch == '@')
}

pub fn destination(value: &str) -> Result<(&str, EndpointId)> {
    let (user, endpoint) = value
        .rsplit_once('@')
        .context("expected USER@ENDPOINT_ID")?;
    ensure!(valid_user(user), "invalid SSH username");
    Ok((user, endpoint.parse().context("invalid iroh Endpoint ID")?))
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub name: String,
    pub user: String,
    pub endpoint: EndpointId,
    #[serde(default)]
    pub relay: String,
}

impl Profile {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.name.is_empty()
                && self.name == self.name.trim()
                && self.name.len() <= 256
                && !self.name.chars().any(char::is_control),
            "the connection name must be 1–256 bytes with no surrounding whitespace or control characters"
        );
        ensure!(valid_user(&self.user), "invalid SSH username");
        if !self.relay.is_empty() {
            let url = crate::relay_url(&self.relay).context("invalid connection relay")?;
            ensure!(
                url.query().is_none() && url.fragment().is_none(),
                "a saved relay URL may not contain a query or fragment"
            );
        }
        Ok(())
    }

    pub fn destination(&self) -> String {
        format!("{}@{}", self.user, self.endpoint)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Backup {
    pub version: u32,
    pub connections: Vec<Profile>,
    #[serde(default)]
    pub history: Vec<String>,
}

impl Default for Backup {
    fn default() -> Self {
        Self {
            version: VERSION,
            connections: Vec::new(),
            history: Vec::new(),
        }
    }
}

impl Backup {
    pub fn new(connections: Vec<Profile>, history: Vec<String>) -> Result<Self> {
        let result = Self {
            connections,
            history,
            ..Self::default()
        };
        result.validate()?;
        Ok(result)
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            self.version == VERSION,
            "unsupported SSHI backup version {}",
            self.version
        );
        let mut names = HashSet::new();
        for profile in &self.connections {
            profile.validate()?;
            ensure!(
                names.insert(profile.name.to_lowercase()),
                "duplicate connection name '{}'",
                profile.name
            );
        }
        for entry in &self.history {
            destination(entry).context("invalid SSHI connection history")?;
        }
        Ok(())
    }

    pub fn read(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
        let mut data = Vec::new();
        file.take(MAX_BYTES + 1).read_to_end(&mut data)?;
        ensure!(
            data.len() as u64 <= MAX_BYTES,
            "connection backup exceeds 16 MiB"
        );
        let backup: Self =
            serde_json::from_slice(&data).context("read SSHI connection backup JSON")?;
        backup.validate()?;
        Ok(backup)
    }

    fn temporary(&self, path: &Path) -> Result<NamedTempFile> {
        self.validate()?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let mut file = NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(&mut file, self)?;
        file.write_all(b"\n")?;
        ensure!(
            file.as_file().metadata()?.len() <= MAX_BYTES,
            "connection backup exceeds 16 MiB"
        );
        file.as_file().sync_all()?;
        Ok(file)
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        self.temporary(path)?
            .persist_noclobber(path)
            .with_context(|| format!("create backup {} (choose a new filename)", path.display()))?;
        Ok(())
    }

    /// The caller holds the connection store lock. The replaced file is always private.
    pub fn replace_store(&self, path: &Path) -> Result<()> {
        self.temporary(path)?
            .persist(path)
            .with_context(|| format!("save connection store {}", path.display()))?;
        #[cfg(unix)]
        if let Some(parent) = path.parent() {
            File::open(parent)?.sync_all()?;
        }
        Ok(())
    }

    /// Merge atomically in memory. A name collision never changes existing credentials or routes.
    pub fn merge(&mut self, incoming: &Self) -> Result<()> {
        incoming.validate()?;
        let mut updated = self.clone();
        let existing: HashMap<_, _> = self
            .connections
            .iter()
            .map(|p| (p.name.to_lowercase(), p))
            .collect();
        for other in &incoming.connections {
            if let Some(current) = existing.get(&other.name.to_lowercase()) {
                ensure!(
                    *current == other,
                    "a different connection is already named '{}'; rename it before restoring",
                    other.name
                );
            } else {
                updated.connections.push(other.clone());
            }
        }
        let mut seen: HashSet<String> = updated.history.iter().cloned().collect();
        updated.history.extend(
            incoming
                .history
                .iter()
                .filter(|s| seen.insert((*s).clone()))
                .cloned(),
        );
        *self = updated;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;

    fn profile(name: &str) -> Profile {
        Profile {
            name: name.into(),
            endpoint: SecretKey::generate().public(),
            user: "pi".into(),
            relay: String::new(),
        }
    }

    #[test]
    fn conflict_preserves_existing_library_and_backup_is_not_overwritten() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("backup.json");
        let old = profile("Work");
        let mut library = Backup::new(vec![old.clone()], vec![old.destination()])?;
        library.write(&path)?;
        let other = Backup::new(vec![profile("Added"), profile("work")], vec![])?;
        assert!(other.write(&path).is_err());
        assert!(library.merge(&other).is_err());
        assert_eq!(library.connections.as_slice(), std::slice::from_ref(&old));
        let backup = Backup::read(&path)?;
        library.merge(&backup)?;
        assert_eq!(library.connections, [old]);
        assert_eq!(library.history.len(), 1);
        Ok(())
    }

    #[test]
    fn refuse_private_fields_and_credentials_in_urls() -> Result<()> {
        let mut p = profile("Desk");
        p.relay = "https://example.org/?token=placeholder".into();
        assert!(p.validate().is_err());
        let bogus = br#"{"version":1,"connections":[],"relay_token":"placeholder"}"#;
        assert!(serde_json::from_slice::<Backup>(bogus).is_err());
        Ok(())
    }
}
