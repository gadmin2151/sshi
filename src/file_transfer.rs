//! Shared SFTP transfers for the CLI and desktop clients.
use std::{
    env,
    io::ErrorKind,
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use clap::Args;
use russh_sftp::{
    client::{Config, SftpSession, error::Error as SftpError},
    protocol::{FileAttributes, OpenFlags, StatusCode},
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
    },
}

impl Request {
    pub fn needs_sftp(&self) -> bool {
        !matches!(
            self,
            Self::List {
                side: Side::Local,
                ..
            }
        )
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
    Listing { listing: Listing },
    Progress { progress: Progress },
    Copied { files: u64 },
    Error { message: String },
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
        } => {
            let sftp = sftp.context("connect to the remote computer first")?;
            ensure!(!names.is_empty(), "select files or folders to copy");
            for name in &names {
                ensure_leaf(name)?;
            }
            let mut copier = Copier::new(sftp, true, |progress| emit(Event::Progress { progress }));
            for name in names {
                let result = match side {
                    Side::Local => {
                        copier
                            .upload(
                                &Path::new(&source).join(&name),
                                &join_remote(&destination, &name),
                                0,
                            )
                            .await
                    }
                    Side::Remote => {
                        copier
                            .download(
                                &join_remote(&source, &name),
                                &Path::new(&destination).join(&name),
                                0,
                            )
                            .await
                    }
                };
                result
                    .with_context(|| format!("copy {name:?} ({} files completed)", copier.files))?;
            }
            Ok(Event::Copied {
                files: copier.files,
            })
        }
    }
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
            if ensure_leaf(&name).is_err() {
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

fn ensure_leaf(name: &str) -> Result<()> {
    let mut parts = Path::new(name).components();
    ensure!(
        !name.contains('/')
            && matches!(parts.next(), Some(Component::Normal(_)))
            && parts.next().is_none(),
        "invalid file name {name:?}"
    );
    #[cfg(windows)]
    ensure!(
        !name.contains(['\\', ':', '\"', '<', '>', '|', '?', '*']) && !name.ends_with([' ', '.']),
        "file name {name:?} is not supported on Windows"
    );
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
    let mut copier = Copier::new(sftp, recursive, progress);
    match transfer {
        Transfer::Upload { local, mut remote } => {
            if remote_metadata(sftp, &remote)
                .await?
                .is_some_and(|info| info.file_type().is_dir())
            {
                remote = join_remote(&remote, local_name(&local)?);
            }
            copier
                .upload(&local, &remote, 0)
                .await
                .with_context(|| format!("copy {local:?} to {remote:?}"))?;
        }
        Transfer::Download { remote, mut local } => {
            if fs::symlink_metadata(&local)
                .await
                .is_ok_and(|info| info.is_dir())
            {
                let name = remote
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap_or_default();
                ensure_leaf(name)?;
                local.push(name);
            }
            copier
                .download(&remote, &local, 0)
                .await
                .with_context(|| format!("copy {remote:?} to {local:?}"))?;
        }
    }
    Ok(copier.files)
}

pub fn print_progress(progress: Progress) {
    if progress.bytes == 0 {
        // Debug formatting keeps terminal control characters out of the CLI.
        eprintln!("Copying {:?}", progress.name);
    }
}

struct Copier<'a, P> {
    sftp: &'a SftpSession,
    recursive: bool,
    files: u64,
    progress: P,
}

impl<'a, P: FnMut(Progress)> Copier<'a, P> {
    fn new(sftp: &'a SftpSession, recursive: bool, progress: P) -> Self {
        Self {
            sftp,
            recursive,
            files: 0,
            progress,
        }
    }

    async fn upload(&mut self, source: &Path, target: &str, depth: usize) -> Result<()> {
        let meta = fs::symlink_metadata(source)
            .await
            .with_context(|| format!("inspect local path {source:?}"))?;
        if meta.is_dir() {
            ensure!(self.recursive, "use -r to copy a folder");
            ensure!(
                depth < 128,
                "nested folders exceed the transfer depth limit"
            );
            self.sftp
                .create_dir(target)
                .await
                .with_context(|| format!("create target folder {target:?}"))?;
            let mut cursor = fs::read_dir(source).await?;
            while let Some(entry) = cursor.next_entry().await? {
                let path = entry.path();
                let remote = join_remote(target, local_name(&path)?);
                Box::pin(self.upload(&path, &remote, depth + 1)).await?;
            }
            return Ok(());
        }
        ensure!(
            meta.is_file(),
            "only regular files and folders can be copied: {source:?}"
        );
        ensure!(
            remote_metadata(self.sftp, target).await?.is_none(),
            "target {target:?} exists; choose a new name"
        );
        let mut input = fs::File::open(source)
            .await
            .with_context(|| format!("read {source:?}"))?;
        let temporary = join_remote(
            remote_parent(target),
            &format!(".sshi-{:032x}.part", rand::random::<u128>()),
        );
        let mut output = self
            .sftp
            .open_with_flags_and_attributes(
                temporary.as_str(),
                OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE,
                FileAttributes {
                    permissions: Some(0o600),
                    ..Default::default()
                },
            )
            .await
            .with_context(|| format!("create temporary file in {:?}", remote_parent(target)))?;
        let result: Result<()> = async {
            let name = source.file_name().unwrap_or_default().to_string_lossy();
            transfer_data(
                &mut input,
                &mut output,
                &name,
                Some(meta.len()),
                &mut self.progress,
            )
            .await?;
            output
                .close()
                .await
                .context("finish writing the remote file")?;
            self.sftp
                .rename(temporary.as_str(), target)
                .await
                .with_context(|| format!("publish {target:?}"))?;
            Ok(())
        }
        .await;
        if result.is_err() {
            let _ = self.sftp.remove_file(temporary).await;
        }
        result?;
        self.files += 1;
        Ok(())
    }

    async fn download(&mut self, source: &str, target: &Path, depth: usize) -> Result<()> {
        let meta = remote_metadata(self.sftp, source)
            .await?
            .with_context(|| format!("remote path {source:?} does not exist"))?;
        if meta.file_type().is_dir() {
            ensure!(self.recursive, "use -r to copy a folder");
            ensure!(
                depth < 128,
                "nested folders exceed the transfer depth limit"
            );
            fs::create_dir(target)
                .await
                .with_context(|| format!("create target folder {target:?}"))?;
            for entry in self.sftp.read_dir(source).await? {
                let name = entry.file_name();
                if matches!(name.as_str(), "." | "..") {
                    continue;
                }
                ensure_leaf(&name)?;
                Box::pin(self.download(
                    &join_remote(source, &name),
                    &target.join(&name),
                    depth + 1,
                ))
                .await?;
            }
            return Ok(());
        }
        ensure!(
            meta.file_type().is_file(),
            "only regular files and folders can be copied: {source:?}"
        );
        match fs::symlink_metadata(target).await {
            Ok(_) => bail!("target {target:?} exists; choose a new name"),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error).with_context(|| format!("inspect {target:?}")),
        }
        let parent = target
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let temporary = tempfile::Builder::new()
            .prefix(".sshi-")
            .suffix(".part")
            .tempfile_in(parent)
            .with_context(|| format!("create temporary file in {parent:?}"))?;
        let mut output = fs::File::from_std(temporary.reopen()?);
        let mut input = self
            .sftp
            .open(source)
            .await
            .with_context(|| format!("read {source:?}"))?;
        let name = source.rsplit('/').next().unwrap_or(source);
        transfer_data(&mut input, &mut output, name, meta.size, &mut self.progress).await?;
        output
            .shutdown()
            .await
            .context("finish writing the local file")?;
        drop(output);
        temporary
            .persist_noclobber(target)
            .with_context(|| format!("publish {target:?}"))?;
        self.files += 1;
        Ok(())
    }
}

async fn transfer_data(
    input: &mut (impl AsyncRead + Unpin),
    output: &mut (impl AsyncWrite + Unpin),
    name: &str,
    total: Option<u64>,
    progress: &mut impl FnMut(Progress),
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
