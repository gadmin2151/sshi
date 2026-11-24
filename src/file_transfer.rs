//! Shared file transfers and file browser operations.
mod editor;
mod operations;

use std::{
    env,
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use clap::Args;
use operations::{Copier, Location};
use russh_sftp::{
    client::{Config, SftpSession, error::Error as SftpError},
    protocol::{FileAttributes, StatusCode},
};
use serde::{Deserialize, Serialize};
use tokio::{
    fs,
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
};

#[derive(Args)]
pub struct CopyArgs {
    /// Copy directories and their contents.
    #[arg(short = 'r', long)]
    pub recursive: bool,
    /// OpenSSH private key file.
    #[arg(short = 'i', value_name = "FILE")]
    pub identity: Option<PathBuf>,
    #[arg(value_name = "SOURCE")]
    pub source: String,
    #[arg(value_name = "DESTINATION")]
    pub destination: String,
}

pub enum Transfer {
    Upload { local: PathBuf, remote: String },
    Download { remote: String, local: PathBuf },
}

impl CopyArgs {
    pub fn transfer(&self) -> Result<(&str, Transfer)> {
        match (split_remote(&self.source), split_remote(&self.destination)) {
            (None, Some((target, remote))) => Ok((
                target,
                Transfer::Upload {
                    local: self.source.clone().into(),
                    remote: remote.into(),
                },
            )),
            (Some((target, remote)), None) => Ok((
                target,
                Transfer::Download {
                    remote: remote.into(),
                    local: self.destination.clone().into(),
                },
            )),
            _ => bail!("use one local path and one PROFILE:PATH or USER@ENDPOINT_ID:PATH"),
        }
    }
}

fn split_remote(value: &str) -> Option<(&str, &str)> {
    let (host, path) = value.split_once(':')?;
    if host.is_empty() || host.contains(['/', '\\']) {
        return None;
    }
    if cfg!(windows) && host.len() == 1 && host.as_bytes()[0].is_ascii_alphabetic() {
        return None;
    }
    Some((host, if path.is_empty() { "." } else { path }))
}

pub async fn open_session<S>(stream: S, timeout_secs: u64) -> Result<SftpSession>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let session = SftpSession::new_with_config(
        stream,
        Config {
            request_timeout_secs: timeout_secs,
            ..Default::default()
        },
    )
    .await
    .context("open SFTP subsystem")?;
    session.set_timeout(35);
    Ok(session)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Local,
    Remote,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Directory,
    File,
    Other,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Entry {
    pub name: String,
    pub kind: Kind,
    pub size: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct Listing {
    pub side: Side,
    pub path: String,
    pub parent: Option<String>,
    pub entries: Vec<Entry>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Request {
    List {
        side: Side,
        path: String,
        #[serde(default)]
        reconnect: bool,
    },
    Copy {
        side: Side,
        source: String,
        destination: String,
        names: Vec<String>,
        #[serde(default)]
        destination_side: Option<Side>,
        #[serde(default)]
        move_files: bool,
    },
    Rename {
        side: Side,
        path: String,
        name: String,
        new_name: String,
    },
    Delete {
        side: Side,
        path: String,
        names: Vec<String>,
    },
    NewFolder {
        side: Side,
        path: String,
        name: String,
    },
    ReadText {
        side: Side,
        path: String,
        name: String,
    },
    SaveText {
        side: Side,
        path: String,
        name: String,
        text: String,
        revision: String,
    },
}

impl Side {
    pub fn other(self) -> Self {
        match self {
            Self::Local => Self::Remote,
            Self::Remote => Self::Local,
        }
    }
}

impl Request {
    pub fn needs_sftp(&self) -> bool {
        match self {
            Self::Copy {
                side,
                destination_side,
                ..
            } => {
                *side == Side::Remote
                    || destination_side.unwrap_or_else(|| side.other()) == Side::Remote
            }
            Self::List { side, .. }
            | Self::Rename { side, .. }
            | Self::Delete { side, .. }
            | Self::NewFolder { side, .. }
            | Self::ReadText { side, .. }
            | Self::SaveText { side, .. } => *side == Side::Remote,
        }
    }

    pub fn reconnects(&self) -> bool {
        matches!(
            self,
            Self::List {
                side: Side::Remote,
                reconnect: true,
                ..
            }
        )
    }
}

#[derive(Debug, Serialize)]
pub struct Progress {
    pub name: String,
    pub bytes: u64,
    pub total: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Listing {
        listing: Listing,
    },
    Progress {
        progress: Progress,
    },
    Copied {
        files: u64,
    },
    Changed {
        entries: usize,
    },
    Text {
        text: String,
        revision: String,
    },
    Saved {
        revision: String,
        warning: Option<String>,
    },
    Error {
        message: String,
    },
}

pub async fn handle(
    request: Request,
    sftp: Option<&SftpSession>,
    mut emit: impl FnMut(Event),
) -> Result<Event> {
    match request {
        Request::List {
            side: Side::Local,
            path,
            ..
        } => Ok(Event::Listing {
            listing: list_local(&path).await?,
        }),
        Request::List { path, .. } => Ok(Event::Listing {
            listing: list_remote(sftp.context("connect to the remote computer first")?, &path)
                .await?,
        }),
        Request::Copy {
            side,
            source,
            destination,
            names,
            destination_side,
            move_files,
        } => {
            let source = Location::directory(side, &source, sftp)?;
            let target = Location::directory(
                destination_side.unwrap_or_else(|| side.other()),
                &destination,
                sftp,
            )?;
            let names = children(&source, names)?;
            let targets = names
                .iter()
                .map(|(name, _)| target.child(name))
                .collect::<Result<Vec<_>>>()?;
            let mut copier = Copier::new(true, |progress| emit(Event::Progress { progress }));
            let entries = names.len();
            for (index, ((name, source), target)) in names.into_iter().zip(targets).enumerate() {
                copier
                    .item(&source, &target, move_files)
                    .await
                    .with_context(|| {
                        let action = if move_files { "move" } else { "copy" };
                        format!("{action} {name:?} ({index} items completed)")
                    })?;
            }
            if move_files {
                Ok(Event::Changed { entries })
            } else {
                Ok(Event::Copied {
                    files: copier.files,
                })
            }
        }
        Request::Rename {
            side,
            path,
            name,
            new_name,
        } => {
            let directory = Location::directory(side, &path, sftp)?;
            let source = directory.child(&name)?;
            let target = directory.child(&new_name)?;
            source.rename_to(&target).await?;
            Ok(Event::Changed { entries: 1 })
        }
        Request::Delete { side, path, names } => {
            let directory = Location::directory(side, &path, sftp)?;
            let names = children(&directory, names)?;
            let entries = names.len();
            for (index, (name, path)) in names.into_iter().enumerate() {
                path.remove_tree(0)
                    .await
                    .with_context(|| format!("delete {name:?} ({index} items completed)"))?;
            }
            Ok(Event::Changed { entries })
        }
        Request::NewFolder { side, path, name } => {
            Location::directory(side, &path, sftp)?
                .child(&name)?
                .create_dir()
                .await?;
            Ok(Event::Changed { entries: 1 })
        }
        Request::ReadText { side, path, name } => {
            editor::read(&Location::directory(side, &path, sftp)?.child(&name)?).await
        }
        Request::SaveText {
            side,
            path,
            name,
            text,
            revision,
        } => {
            editor::save(
                &Location::directory(side, &path, sftp)?.child(&name)?,
                &revision,
                &text,
            )
            .await
        }
    }
}

fn children<'a>(
    directory: &Location<'a>,
    names: Vec<String>,
) -> Result<Vec<(String, Location<'a>)>> {
    ensure!(!names.is_empty(), "select one or more files or folders");
    let mut seen = std::collections::HashSet::new();
    names
        .into_iter()
        .map(|name| {
            ensure!(
                seen.insert(name.clone()),
                "file name {name:?} was selected more than once"
            );
            let path = directory.child(&name)?;
            Ok((name, path))
        })
        .collect()
}

async fn list_local(path: &str) -> Result<Listing> {
    let path = if path.is_empty() {
        PathBuf::from(
            env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .context("find the home directory")?,
        )
    } else {
        PathBuf::from(path)
    };
    let directory = fs::canonicalize(&path)
        .await
        .with_context(|| format!("open local folder {path:?}"))?;
    let mut cursor = fs::read_dir(&directory)
        .await
        .with_context(|| format!("list local folder {directory:?}"))?;
    let mut entries = Vec::new();
    while let Some(item) = cursor.next_entry().await? {
        // The exchange uses UTF-8, so omit unrepresentable names.
        let Ok(name) = item.file_name().into_string() else {
            continue;
        };
        let file_type = item.file_type().await?;
        let kind = if file_type.is_dir() {
            Kind::Directory
        } else if file_type.is_file() {
            Kind::File
        } else {
            Kind::Other
        };
        let size = if kind == Kind::File {
            item.metadata().await.ok().map(|info| info.len())
        } else {
            None
        };
        entries.push(Entry { name, kind, size });
    }
    sort_entries(&mut entries);
    Ok(Listing {
        side: Side::Local,
        parent: directory.parent().map(display_local),
        path: display_local(&directory),
        entries,
    })
}

fn display_local(path: &Path) -> String {
    let text = path.to_string_lossy();
    #[cfg(windows)]
    {
        if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
            return format!(r"\\{unc}");
        }
        if let Some(drive) = text.strip_prefix(r"\\?\")
            && drive.as_bytes().get(1) == Some(&b':')
        {
            return drive.to_owned();
        }
    }
    text.into_owned()
}

async fn list_remote(sftp: &SftpSession, path: &str) -> Result<Listing> {
    let path = sftp
        .canonicalize(if path.is_empty() { "." } else { path })
        .await
        .with_context(|| format!("open remote folder {path:?}"))?;
    ensure!(
        path.starts_with('/'),
        "the SFTP server did not return an absolute folder path"
    );
    let mut entries = sftp
        .read_dir(path.as_str())
        .await
        .with_context(|| format!("list remote folder {path:?}"))?
        .filter_map(|item| {
            let name = item.file_name();
            if ensure_remote_name(&name).is_err() {
                return None;
            }
            let meta = item.metadata();
            let kind = remote_kind(&meta);
            Some(Entry {
                name,
                kind,
                size: meta.size.filter(|_| kind == Kind::File),
            })
        })
        .collect::<Vec<_>>();
    sort_entries(&mut entries);
    let parent = if path == "/" {
        None
    } else {
        Some(remote_parent(&path).to_owned())
    };
    Ok(Listing {
        side: Side::Remote,
        path,
        parent,
        entries,
    })
}

fn sort_entries(entries: &mut [Entry]) {
    entries.sort_by_cached_key(|e| (e.kind != Kind::Directory, e.name.to_lowercase()));
}

fn remote_kind(meta: &FileAttributes) -> Kind {
    let kind = meta.file_type();
    if kind.is_dir() {
        Kind::Directory
    } else if kind.is_file() {
        Kind::File
    } else {
        Kind::Other
    }
}

fn ensure_remote_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && !name.contains(['/', '\0']) && !matches!(name, "." | ".."),
        "invalid file name {name:?}"
    );
    Ok(())
}

fn ensure_leaf(name: &str) -> Result<()> {
    ensure_remote_name(name)?;
    let mut parts = Path::new(name).components();
    ensure!(
        !name.contains('/')
            && matches!(parts.next(), Some(Component::Normal(_)))
            && parts.next().is_none(),
        "invalid file name {name:?}"
    );
    #[cfg(windows)]
    {
        let stem = name
            .split('.')
            .next()
            .unwrap_or_default()
            .trim_end()
            .to_ascii_uppercase();
        let device = matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        ) || (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(
                &stem[3..],
                "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            );
        ensure!(
            !device
                && !name
                    .chars()
                    .any(|ch| ch < ' '
                        || matches!(ch, '\\' | ':' | '"' | '<' | '>' | '|' | '?' | '*'))
                && !name.ends_with([' ', '.']),
            "file name {name:?} is not supported on Windows"
        );
    }
    Ok(())
}

fn local_name(path: &Path) -> Result<&str> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("use a file or folder name encoded in UTF-8")?;
    ensure_leaf(name)?;
    Ok(name)
}

fn join_remote(directory: &str, name: &str) -> String {
    format!("{}/{name}", directory.trim_end_matches('/'))
}

fn remote_parent(path: &str) -> &str {
    match path.trim_end_matches('/').rsplit_once('/') {
        Some(("", _)) => "/",
        Some((directory, _)) => directory,
        None => ".",
    }
}

async fn remote_metadata(sftp: &SftpSession, path: &str) -> Result<Option<FileAttributes>> {
    match sftp.symlink_metadata(path).await {
        Ok(meta) => Ok(Some(meta)),
        Err(SftpError::Status(status)) if status.status_code == StatusCode::NoSuchFile => Ok(None),
        Err(error) => Err(error).with_context(|| format!("inspect remote path {path:?}")),
    }
}

pub async fn copy(
    sftp: &SftpSession,
    transfer: Transfer,
    recursive: bool,
    progress: impl FnMut(Progress),
) -> Result<u64> {
    let mut copier = Copier::new(recursive, progress);
    let (source, target) = match transfer {
        Transfer::Upload { local, remote } => {
            let mut target = Location::Remote(sftp, remote);
            if target.kind().await? == Some(Kind::Directory) {
                target = target.child(local_name(&local)?)?;
            }
            (Location::Local(local), target)
        }
        Transfer::Download { remote, local } => {
            let mut target = Location::Local(local);
            if target.kind().await? == Some(Kind::Directory) {
                let name = remote
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap_or_default();
                target = target.child(name)?;
            }
            (Location::Remote(sftp, remote), target)
        }
    };
    copier
        .item(&source, &target, false)
        .await
        .with_context(|| format!("copy {source} to {target}"))?;
    Ok(copier.files)
}

pub fn print_progress(progress: Progress) {
    if progress.bytes == 0 {
        // Debug formatting keeps terminal control characters out of the CLI.
        eprintln!("Copying {:?}", progress.name);
    }
}

async fn transfer_data(
    input: &mut (impl AsyncRead + Unpin),
    output: &mut (impl AsyncWrite + Unpin),
    name: &str,
    total: Option<u64>,
    progress: &mut impl FnMut(Progress),
    mut digest: Option<&mut blake3::Hasher>,
) -> Result<()> {
    let mut bytes = 0;
    let mut status = Instant::now();
    let mut buffer = vec![0; 256 * 1024];
    progress(Progress {
        name: name.into(),
        bytes,
        total,
    });
    loop {
        let read = input.read(&mut buffer).await.context("read file data")?;
        if read == 0 {
            break;
        }
        output
            .write_all(&buffer[..read])
            .await
            .context("write file data")?;
        if let Some(digest) = digest.as_deref_mut() {
            digest.update(&buffer[..read]);
        }
        bytes += read as u64;
        if status.elapsed() >= Duration::from_millis(180) {
            progress(Progress {
                name: name.into(),
                bytes,
                total,
            });
            status = Instant::now();
        }
    }
    if let Some(total) = total {
        ensure!(bytes == total, "file size changed during transfer");
    }
    Ok(())
}
