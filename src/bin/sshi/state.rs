use std::{
    collections::HashSet,
    env,
    fs::{self, DirBuilder, File, OpenOptions},
    io::{self, ErrorKind, Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use iroh::EndpointId;
use iroh_ssh::profiles::Backup;

use crate::invitations::Invitations;

pub struct State(PathBuf);

impl State {
    pub fn load() -> Result<Self> {
        Ok(Self(
            xdg_dir("XDG_STATE_HOME", ".local/state")?.join("sshi"),
        ))
    }

    pub fn id(&self) -> Result<EndpointId> {
        self.prepare()?;
        let path = self.key();
        if !path.try_exists().context("check for the sshi key")? {
            match iroh_ssh::generate_key(&path) {
                Ok(id) => return Ok(id),
                Err(error)
                    if error
                        .downcast_ref::<io::Error>()
                        .is_some_and(|err| err.kind() == ErrorKind::AlreadyExists) => {}
                Err(error) => return Err(error),
            }
        }
        iroh_ssh::key_id(&path)
    }

    pub fn key(&self) -> PathBuf {
        self.0.join("endpoint.key")
    }

    pub fn invitations(&self) -> Result<Invitations> {
        Ok(Invitations::new(&self.0, self.id()?))
    }

    pub fn socket(&self) -> PathBuf {
        self.0.join("proxy.sock")
    }

    pub fn broker_start_lock(&self) -> Result<File> {
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(self.0.join("broker-start.lock"))
            .context("open local sshi broker startup lock")?;
        lock.lock().context("lock local sshi broker startup")?;
        Ok(lock)
    }

    fn recent_history(&self) -> Result<Vec<String>> {
        let mut file = match File::open(self.0.join("history")) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error).context("open sshi history"),
        };
        file.lock_shared().context("lock sshi history")?;
        let mut history = String::new();
        file.read_to_string(&mut history)
            .context("read sshi history")?;
        drop(file);
        let mut listed = HashSet::new();
        Ok(history
            .lines()
            .rev()
            .filter(|destination| listed.insert(*destination))
            .map(str::to_owned)
            .collect())
    }

    pub fn library(&self) -> Result<Backup> {
        let path = self.0.join("connections.json");
        if !path.try_exists()? {
            return Ok(Backup::default());
        }
        Backup::read(&path)
    }

    pub fn snapshot(&self) -> Result<Backup> {
        let mut library = self.library()?;
        let mut history = self.recent_history()?;
        let mut seen: HashSet<_> = history.iter().cloned().collect();
        history.extend(
            library
                .history
                .into_iter()
                .filter(|s| seen.insert(s.clone())),
        );
        library.history = history;
        Ok(library)
    }

    pub fn list(&self) -> Result<()> {
        let mut out = io::stdout().lock();
        for entry in self.snapshot()?.history {
            writeln!(out, "{entry}")?;
        }
        Ok(())
    }

    pub fn update_library(&self, update: impl FnOnce(&mut Backup) -> Result<()>) -> Result<()> {
        self.prepare()?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(self.0.join("connections.lock"))?;
        lock.lock().context("lock connections")?;
        let mut library = self.library()?;
        update(&mut library)?;
        library.replace_store(&self.0.join("connections.json"))
    }

    pub fn record(&self, destination: &str) -> Result<()> {
        self.prepare()?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(self.0.join("history"))
            .context("open sshi history")?;
        file.lock().context("lock sshi history")?;
        writeln!(file, "{destination}").context("write sshi history")
    }

    fn prepare(&self) -> Result<()> {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.0)
            .with_context(|| format!("create {}", self.0.display()))?;
        let metadata = fs::symlink_metadata(&self.0).context("inspect sshi state directory")?;
        ensure!(
            metadata.is_dir() && metadata.permissions().mode() & 0o077 == 0,
            "{} must be a private directory (mode 700)",
            self.0.display()
        );
        Ok(())
    }
}

pub fn xdg_dir(variable: &str, fallback: &str) -> Result<PathBuf> {
    if let Some(path) = env::var_os(variable).filter(|path| Path::new(path).is_absolute()) {
        return Ok(path.into());
    }
    let home = env::var_os("HOME")
        .filter(|path| Path::new(path).is_absolute())
        .context("set HOME to an absolute path")?;
    Ok(PathBuf::from(home).join(fallback))
}
