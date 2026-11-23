use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{ErrorKind, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, ensure};
use iroh::EndpointId;
use iroh_ssh::invite::Code;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

const VALID_FOR: Duration = Duration::from_secs(10 * 60);

#[derive(Clone)]
pub struct Invitations {
    directory: PathBuf,
    receiver: EndpointId,
}

#[derive(Default, Deserialize, Serialize)]
struct Access {
    clients: BTreeMap<String, Grant>,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Grant {
    Pending { fingerprint: String, expires: u64 },
    Active { fingerprint: String },
}

impl Invitations {
    pub fn new(directory: &Path, receiver: EndpointId) -> Self {
        Self {
            directory: directory.to_owned(),
            receiver,
        }
    }

    pub fn issue(&self, client: EndpointId) -> Result<Code> {
        let _lock = self.lock()?;
        let mut access = self.load()?;
        let id = client.to_string();
        ensure!(
            !matches!(access.clients.get(&id), Some(Grant::Active { .. })),
            "Endpoint ID {client} already has invitation access"
        );
        let code = Code::generate();
        access.clients.insert(
            id,
            Grant::Pending {
                fingerprint: code.fingerprint().to_string(),
                expires: now()? + VALID_FOR.as_secs(),
            },
        );
        self.save(&access)?;
        Ok(code)
    }

    pub fn redeem(&self, client: EndpointId, code: Code) -> Result<bool> {
        let _lock = self.lock()?;
        let mut access = self.load()?;
        let id = client.to_string();
        let Some(grant) = access.clients.get(&id) else {
            return Ok(false);
        };
        let offered = code.fingerprint();
        match grant {
            // A lost response can be retried by the same authenticated Endpoint ID.
            Grant::Active { fingerprint } => Ok(matches_hash(fingerprint, offered)),
            Grant::Pending {
                fingerprint,
                expires,
            } => {
                if now()? >= *expires || !matches_hash(fingerprint, offered) {
                    return Ok(false);
                }
                access.clients.insert(
                    id,
                    Grant::Active {
                        fingerprint: offered.to_string(),
                    },
                );
                self.save(&access)?;
                Ok(true)
            }
        }
    }

    pub fn allows(&self, client: EndpointId) -> Result<bool> {
        Ok(matches!(
            self.load()?.clients.get(&client.to_string()),
            Some(Grant::Active { .. })
        ))
    }

    pub fn revoke(&self, client: EndpointId) -> Result<bool> {
        let _lock = self.lock()?;
        let mut access = self.load()?;
        let removed = access.clients.remove(&client.to_string()).is_some();
        if removed {
            self.save(&access)?;
        }
        Ok(removed)
    }

    fn file(&self) -> PathBuf {
        self.directory
            .join(format!("invites-{}.toml", self.receiver))
    }

    fn lock(&self) -> Result<File> {
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(
                self.directory
                    .join(format!("invites-{}.lock", self.receiver)),
            )
            .context("open the invitation lock")?;
        lock.lock().context("lock invitations")?;
        Ok(lock)
    }

    fn load(&self) -> Result<Access> {
        match fs::read(self.file()) {
            Ok(data) => toml::from_slice(&data)
                .map_err(|error| anyhow::anyhow!("invalid invitation state ({})", error.message())),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(Access::default()),
            Err(error) => Err(error).context("read invitation state"),
        }
    }

    fn save(&self, access: &Access) -> Result<()> {
        let mut file = NamedTempFile::new_in(&self.directory)?;
        file.write_all(toml::to_string(access)?.as_bytes())?;
        file.as_file().sync_all()?;
        file.persist(self.file()).context("save invitation state")?;
        File::open(&self.directory)?.sync_all()?;
        Ok(())
    }
}

fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

fn matches_hash(expected: &str, offered: blake3::Hash) -> bool {
    blake3::Hash::from_hex(expected).is_ok_and(|hash| hash == offered)
}

#[cfg(test)]
mod tests {
    use iroh::SecretKey;

    use super::*;

    #[test]
    fn grants_access_only_to_the_invitations_endpoint() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let receiver = SecretKey::generate().public();
        let client = SecretKey::generate().public();
        let other = SecretKey::generate().public();
        let invitations = Invitations::new(directory.path(), receiver);
        let code = invitations.issue(client)?.to_string();

        assert!(!invitations.allows(client)?);
        assert!(!invitations.redeem(other, code.parse()?)?);
        assert!(!invitations.redeem(client, Code::generate())?);
        assert!(invitations.redeem(client, code.parse()?)?);
        assert!(invitations.allows(client)?);
        assert!(invitations.redeem(client, code.parse()?)?);
        assert!(!invitations.redeem(other, code.parse()?)?);

        let reloaded = Invitations::new(directory.path(), receiver);
        assert!(reloaded.revoke(client)?);
        assert!(!reloaded.allows(client)?);
        assert!(!reloaded.redeem(client, code.parse()?)?);
        Ok(())
    }

    #[test]
    fn expired_invitation_cannot_be_redeemed() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let invitations = Invitations::new(directory.path(), SecretKey::generate().public());
        let client = SecretKey::generate().public();
        let code = Code::generate();
        let mut access = Access::default();
        access.clients.insert(
            client.to_string(),
            Grant::Pending {
                fingerprint: code.fingerprint().to_string(),
                expires: 0,
            },
        );
        invitations.save(&access)?;
        assert!(!invitations.redeem(client, code)?);
        assert!(!invitations.allows(client)?);
        Ok(())
    }
}
