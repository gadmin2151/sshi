use crate::lang::{tr, tr_format};
use std::{
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions, TryLockError},
    io::{ErrorKind, Write},
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    ptr::{null, null_mut},
    slice,
};

use anyhow::{Context, Result, bail, ensure};
use iroh::{EndpointId, SecretKey};
use iroh_ssh::profiles::{Backup, Profile as PortableProfile};
use russh::keys::{Algorithm, PrivateKey, ssh_key::LineEnding};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use windows_sys::Win32::{
    Foundation::{GetLastError, LocalFree},
    Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    },
    Storage::FileSystem::{
        FILE_ATTRIBUTE_HIDDEN, GetFileAttributesW, INVALID_FILE_ATTRIBUTES, SetFileAttributesW,
    },
    System::Com::CoTaskMemFree,
    UI::Shell::{FOLDERID_Documents, KF_FLAG_CREATE, SHGetKnownFolderPath},
};

pub const DEFAULT_RELAY: &str = "https://iroh1.cmon.systems/";

#[derive(Clone, Deserialize, Serialize)]
pub struct Profile {
    pub name: String,
    pub endpoint: EndpointId,
    pub user: String,
    pub relay: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ssh_key: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relay_token: Vec<u8>,
}

impl Profile {
    pub fn portable(&self) -> PortableProfile {
        PortableProfile {
            name: self.name.clone(),
            endpoint: self.endpoint,
            user: self.user.clone(),
            relay: self.relay.clone(),
        }
    }
}

#[derive(Default, Deserialize, Serialize)]
struct Profiles {
    #[serde(default)]
    connections: Vec<Profile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    history: Vec<String>,
}

impl Profiles {
    fn portable(&self) -> Result<Backup> {
        Backup::new(
            self.connections.iter().map(Profile::portable).collect(),
            self.history.clone(),
        )
    }
}

#[derive(Clone)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn open() -> Result<Self> {
        let mut documents = null_mut();
        // Use the Windows known folder so redirected and localized Documents work.
        let status = unsafe {
            SHGetKnownFolderPath(
                &FOLDERID_Documents,
                KF_FLAG_CREATE as u32,
                null_mut(),
                &mut documents,
            )
        };
        ensure!(
            status >= 0 && !documents.is_null(),
            "{}",
            tr_format!(
                "Не удалось найти папку «Документы» (0x{status:08x})",
                "Could not find the Documents folder (0x{status:08x})"
            )
        );
        let path = unsafe {
            let mut len = 0;
            while *documents.add(len) != 0 {
                len += 1;
            }
            let path = OsString::from_wide(slice::from_raw_parts(documents, len));
            CoTaskMemFree(documents.cast());
            path
        };
        let dir = PathBuf::from(path).join(".sshi");
        fs::create_dir_all(&dir).context(tr(
            "Создать папку «Документы»\\.sshi",
            "Create the Documents\\.sshi folder",
        ))?;
        let path = wide(&dir);
        let attributes = unsafe { GetFileAttributesW(path.as_ptr()) };
        if attributes == INVALID_FILE_ATTRIBUTES
            || unsafe { SetFileAttributesW(path.as_ptr(), attributes | FILE_ATTRIBUTE_HIDDEN) } == 0
        {
            return Err(last_error()).context(tr("Скрыть папку .sshi", "Hide the .sshi folder"));
        }
        match fs::read_to_string(dir.join("language.txt")) {
            Ok(value) => crate::lang::set_english(value.trim() == "en"),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("Read the SSHI language preference"),
        }
        Ok(Self { dir })
    }

    pub fn set_english(&self, english: bool) -> Result<()> {
        self.temp_file(if english { b"en\n" } else { b"ru\n" })?
            .persist(self.dir.join("language.txt"))
            .context(tr(
                "Сохранить выбранный язык",
                "Save the SSHI language preference",
            ))?;
        crate::lang::set_english(english);
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.dir
    }

    pub fn profiles(&self) -> Result<Vec<Profile>> {
        Ok(self.read_profiles()?.connections)
    }

    fn read_profiles(&self) -> Result<Profiles> {
        match fs::read_to_string(self.dir.join("connections.toml")) {
            Ok(text) => toml::from_str(&text).context(tr(
                "Прочитать список подключений",
                "Read the connection list",
            )),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(Profiles::default()),
            Err(error) => Err(error).context(tr(
                "Прочитать список подключений",
                "Read the connection list",
            )),
        }
    }

    pub fn save_profiles(&self, connections: &[Profile]) -> Result<()> {
        let _lock = self.lock("profiles.lock", false)?;
        let mut profiles = self.read_profiles()?;
        profiles.connections = connections.to_vec();
        self.write_profiles(&profiles)
    }

    fn write_profiles(&self, profiles: &Profiles) -> Result<()> {
        let text = toml::to_string_pretty(profiles)?;
        let file = self.temp_file(text.as_bytes())?;
        file.persist(self.dir.join("connections.toml")).context(tr(
            "Сохранить список подключений",
            "Save the connection list",
        ))?;
        Ok(())
    }

    pub fn backup(&self, path: &Path) -> Result<()> {
        self.read_profiles()?.portable()?.write(path)
    }

    pub fn restore(&self, path: &Path) -> Result<usize> {
        let backup = Backup::read(path)?;
        let _lock = self.lock("profiles.lock", false)?;
        let mut state = self.read_profiles()?;
        let before = state.connections.len();
        let mut library = state.portable()?;
        library.merge(&backup)?;
        // merge appends new profiles after existing profiles. Leave local secrets
        // attached to their original profiles and carry only portable metadata in.
        state.connections.extend(
            library
                .connections
                .into_iter()
                .skip(before)
                .map(|incoming| Profile {
                    name: incoming.name,
                    endpoint: incoming.endpoint,
                    user: incoming.user,
                    relay: incoming.relay,
                    ssh_key: String::new(),
                    relay_token: Vec::new(),
                }),
        );
        state.history = library.history;
        self.write_profiles(&state)?;
        Ok(state.connections.len() - before)
    }

    pub fn record_connection(&self, profile: &Profile) -> Result<()> {
        let _lock = self.lock("profiles.lock", false)?;
        let mut state = self.read_profiles()?;
        let destination = format!("{}@{}", profile.user, profile.endpoint);
        if !state.history.contains(&destination) {
            state.history.push(destination);
            self.write_profiles(&state)?;
        }
        Ok(())
    }

    pub fn history(&self) -> Result<Vec<String>> {
        Ok(self.read_profiles()?.history)
    }

    pub fn identity(&self) -> Result<SecretKey> {
        let _lock = self.lock("identity.lock", false)?;
        let path = self.dir.join("identity.dat");
        match fs::read(&path) {
            Ok(data) => {
                let secret = crypt(&data, false).context(tr(
                    "Расшифровать ключ iroh в профиле Windows",
                    "Decrypt the Iroh key in the Windows profile",
                ))?;
                let bytes: [u8; 32] = secret.try_into().map_err(|_| {
                    anyhow::anyhow!(tr("Некорректный ключ iroh", "Invalid Iroh key"))
                })?;
                Ok(SecretKey::from_bytes(&bytes))
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                let key = SecretKey::generate();
                let data = crypt(&key.to_bytes(), true)
                    .context(tr("Защитить ключ iroh", "Protect the Iroh key"))?;
                self.temp_file(&data)?
                    .persist_noclobber(path)
                    .context(tr("Сохранить ключ iroh", "Save the Iroh key"))?;
                Ok(key)
            }
            Err(error) => Err(error).context(tr("Прочитать ключ iroh", "Read the Iroh key")),
        }
    }

    pub fn ssh_identity(&self) -> Result<Option<PrivateKey>> {
        match fs::read(self.dir.join("ssh-identity.dat")) {
            Ok(data) => {
                let private = crypt(&data, false).context(tr(
                    "Расшифровать сохранённый SSH-ключ",
                    "Decrypt the saved SSH key",
                ))?;
                let key = PrivateKey::from_openssh(private).context(tr(
                    "Прочитать сохранённый SSH-ключ",
                    "Read the saved SSH key",
                ))?;
                Ok(Some(key))
            }
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => {
                Err(error).context(tr("Открыть сохранённый SSH-ключ", "Open the saved SSH key"))
            }
        }
    }

    pub fn ensure_ssh_identity(&self) -> Result<PrivateKey> {
        let _lock = self.lock("ssh-identity.lock", false)?;
        if let Some(key) = self.ssh_identity()? {
            return Ok(key);
        }
        let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)
            .context(tr("Создать SSH-ключ Ed25519", "Create an Ed25519 SSH key"))?;
        let private = key.to_openssh(LineEnding::LF)?;
        let protected = crypt(private.as_bytes(), true)
            .context(tr("Защитить SSH-ключ", "Protect the SSH key"))?;
        self.temp_file(&protected)?
            .persist_noclobber(self.dir.join("ssh-identity.dat"))
            .context(tr("Сохранить SSH-ключ", "Save the SSH key"))?;
        Ok(key)
    }

    pub fn lock(&self, name: &str, immediate: bool) -> Result<File> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.dir.join(name))?;
        if immediate {
            match file.try_lock() {
                Ok(()) => {}
                Err(TryLockError::WouldBlock) => {
                    bail!(tr(
                        "Этот экземпляр sshi уже используется в другом окне",
                        "This SSHI instance is already in use in another window"
                    ))
                }
                Err(TryLockError::Error(error)) => {
                    return Err(error)
                        .context(tr("Заблокировать файл состояния", "Lock the state file"));
                }
            }
        } else {
            file.lock()
                .context(tr("Заблокировать файл состояния", "Lock the state file"))?;
        }
        Ok(file)
    }

    pub fn host_fingerprint(&self, endpoint: EndpointId) -> Result<Option<String>> {
        match fs::read_to_string(self.dir.join(format!("host-{endpoint}.txt"))) {
            Ok(value) => Ok(Some(value.trim().to_owned())),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).context(tr(
                "Прочитать отпечаток SSH-сервера",
                "Read the SSH host key fingerprint",
            )),
        }
    }

    pub fn save_host_fingerprint(&self, endpoint: EndpointId, fingerprint: &str) -> Result<()> {
        let path = self.dir.join(format!("host-{endpoint}.txt"));
        match self
            .temp_file(fingerprint.as_bytes())?
            .persist_noclobber(path)
        {
            Ok(_) => Ok(()),
            Err(error) if error.error.kind() == ErrorKind::AlreadyExists => {
                ensure!(
                    self.host_fingerprint(endpoint)?.as_deref() == Some(fingerprint),
                    tr(
                        "Ключ SSH-сервера отличается от отпечатка, сохранённого другим окном",
                        "The SSH host key differs from the fingerprint saved by another window"
                    )
                );
                Ok(())
            }
            Err(error) => Err(error).context(tr(
                "Сохранить отпечаток SSH-сервера",
                "Save the SSH host key fingerprint",
            )),
        }
    }

    pub fn temp_file(&self, data: &[u8]) -> Result<NamedTempFile> {
        let mut file = NamedTempFile::new_in(&self.dir)?;
        file.write_all(data)?;
        file.as_file().sync_all()?;
        Ok(file)
    }
}

pub fn crypt(input: &[u8], protect: bool) -> Result<Vec<u8>> {
    if input.is_empty() {
        return Ok(Vec::new());
    }
    let source = CRYPT_INTEGER_BLOB {
        cbData: input
            .len()
            .try_into()
            .context(tr("Данные слишком велики", "The data is too large"))?,
        pbData: input.as_ptr().cast_mut(),
    };
    let mut dest = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: null_mut(),
    };
    let result = unsafe {
        if protect {
            CryptProtectData(
                &source,
                null(),
                null(),
                null(),
                null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut dest,
            )
        } else {
            CryptUnprotectData(
                &source,
                null_mut(),
                null(),
                null(),
                null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut dest,
            )
        }
    };
    if result == 0 {
        return Err(last_error()).context("Windows DPAPI");
    }
    let data = unsafe {
        let data = slice::from_raw_parts(dest.pbData, dest.cbData as usize).to_vec();
        LocalFree(dest.pbData.cast());
        data
    };
    Ok(data)
}

pub fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}

pub fn last_error() -> std::io::Error {
    std::io::Error::from_raw_os_error(unsafe { GetLastError() } as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_backup_preserves_local_credentials_and_merges_history() -> Result<()> {
        let source = tempfile::tempdir()?;
        let other = tempfile::tempdir()?;
        let from = Store {
            dir: source.path().into(),
        };
        let to = Store {
            dir: other.path().into(),
        };
        let profile = Profile {
            name: "Студия".into(),
            endpoint: SecretKey::generate().public(),
            user: "pi".into(),
            relay: "https://example.org/".into(),
            ssh_key: r"C:\Keys\example".into(),
            relay_token: vec![1, 2, 3],
        };
        from.save_profiles(std::slice::from_ref(&profile))?;
        from.record_connection(&profile)?;
        let file = source.path().join("backup.json");
        from.backup(&file)?;
        assert!(from.backup(&file).is_err());
        let raw = fs::read_to_string(&file)?;
        assert!(!raw.contains("ssh_key") && !raw.contains("relay_token") && !raw.contains("Keys"));

        assert_eq!(from.restore(&file)?, 0);
        let existing = from.profiles()?.remove(0);
        assert_eq!(existing.ssh_key, profile.ssh_key);
        assert_eq!(existing.relay_token, profile.relay_token);
        assert_eq!(to.restore(&file)?, 1);
        assert_eq!(to.restore(&file)?, 0);
        let imported = to.profiles()?.remove(0);
        assert_eq!(imported.portable(), profile.portable());
        assert!(imported.ssh_key.is_empty() && imported.relay_token.is_empty());
        assert_eq!(to.history()?, from.history()?);

        let mut conflict = Backup::read(&file)?;
        conflict.connections[0].user = "other".into();
        let second = source.path().join("conflict.json");
        conflict.write(&second)?;
        assert!(to.restore(&second).is_err());
        assert_eq!(to.profiles()?.remove(0).portable(), imported.portable());
        Ok(())
    }
}
