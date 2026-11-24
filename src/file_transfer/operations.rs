use std::{
    fmt, io,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use russh_sftp::{
    client::{SftpSession, fs::File as RemoteFile},
    protocol::{FileAttributes, OpenFlags},
};
use tokio::{
    fs,
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
};

use super::{
    Kind, Progress, Side, ensure_leaf, ensure_remote_name, join_remote, remote_kind,
    remote_metadata, remote_parent, transfer_data,
};

#[derive(Clone)]
pub(super) enum Location<'a> {
    Local(PathBuf),
    Remote(&'a SftpSession, String),
}

impl fmt::Display for Location<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local(path) => write!(f, "{path:?}"),
            Self::Remote(_, path) => write!(f, "{path:?}"),
        }
    }
}

impl<'a> Location<'a> {
    pub(super) fn directory(side: Side, path: &str, sftp: Option<&'a SftpSession>) -> Result<Self> {
        match side {
            Side::Local => {
                ensure!(Path::new(path).is_absolute(), "select a local folder first");
                Ok(Self::Local(path.into()))
            }
            Side::Remote => {
                ensure!(path.starts_with('/'), "select a remote folder first");
                Ok(Self::Remote(
                    sftp.context("connect to the remote computer first")?,
                    path.into(),
                ))
            }
        }
    }

    pub(super) fn child(&self, name: &str) -> Result<Self> {
        match self {
            Self::Local(path) => {
                ensure_leaf(name)?;
                Ok(Self::Local(path.join(name)))
            }
            Self::Remote(sftp, path) => {
                ensure_remote_name(name)?;
                Ok(Self::Remote(sftp, join_remote(path, name)))
            }
        }
    }

    fn name(&self) -> String {
        match self {
            Self::Local(path) => path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into(),
            Self::Remote(_, path) => path.rsplit('/').next().unwrap_or_default().into(),
        }
    }

    pub(super) fn parent(&self) -> Self {
        match self {
            Self::Local(path) => Self::Local(
                path.parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(Path::new("."))
                    .into(),
            ),
            Self::Remote(sftp, path) => Self::Remote(sftp, remote_parent(path).into()),
        }
    }

    pub(super) fn temporary(&self, suffix: &str) -> Result<Self> {
        self.parent()
            .child(&format!(".sshi-{:032x}.{suffix}", rand::random::<u128>()))
    }

    fn contains(&self, target: &Self) -> bool {
        match (self, target) {
            (Self::Local(source), Self::Local(target)) => target.starts_with(source),
            (Self::Remote(_, source), Self::Remote(_, target)) => {
                source == target
                    || target
                        .strip_prefix(source.trim_end_matches('/'))
                        .is_some_and(|end| end.starts_with('/'))
            }
            _ => false,
        }
    }

    async fn details(&self) -> Result<Option<(Kind, Option<u64>)>> {
        Ok(match self {
            Self::Local(path) => match fs::symlink_metadata(path).await {
                Ok(meta) => Some((
                    if meta.is_dir() {
                        Kind::Directory
                    } else if meta.is_file() {
                        Kind::File
                    } else {
                        Kind::Other
                    },
                    Some(meta.len()),
                )),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => return Err(error).with_context(|| format!("inspect {self}")),
            },
            Self::Remote(sftp, path) => remote_metadata(sftp, path)
                .await?
                .map(|meta| (remote_kind(&meta), meta.size)),
        })
    }

    pub(super) async fn kind(&self) -> Result<Option<Kind>> {
        Ok(self.details().await?.map(|(kind, _)| kind))
    }

    pub(super) async fn expect_file(&self) -> Result<()> {
        ensure!(
            self.kind().await? == Some(Kind::File),
            "expected a regular file at {self}"
        );
        Ok(())
    }

    async fn ensure_absent(&self) -> Result<()> {
        ensure!(
            self.kind().await?.is_none(),
            "target {self} exists; choose a different name or folder"
        );
        Ok(())
    }

    async fn entries(&self) -> Result<Vec<String>> {
        let names = match self {
            Self::Local(path) => {
                let mut cursor = fs::read_dir(path)
                    .await
                    .with_context(|| format!("open folder {self}"))?;
                let mut names = Vec::new();
                while let Some(entry) = cursor.next_entry().await? {
                    names.push(
                        entry
                            .file_name()
                            .into_string()
                            .ok()
                            .context("a folder entry name is not valid UTF-8")?,
                    );
                }
                names
            }
            Self::Remote(sftp, path) => sftp
                .read_dir(path.as_str())
                .await?
                .map(|entry| entry.file_name())
                .filter(|name| !matches!(name.as_str(), "." | ".."))
                .collect(),
        };
        Ok(names)
    }

    pub(super) async fn create_dir(&self) -> Result<()> {
        match self {
            Self::Local(path) => fs::create_dir(path)
                .await
                .with_context(|| format!("create folder {self}"))?,
            Self::Remote(sftp, path) => sftp
                .create_dir(path.as_str())
                .await
                .with_context(|| format!("create folder {self}"))?,
        }
        Ok(())
    }

    pub(super) async fn reader(&self) -> Result<Box<dyn AsyncRead + Unpin + Send>> {
        match self {
            Self::Local(path) => Ok(Box::new(
                fs::File::open(path)
                    .await
                    .with_context(|| format!("read {self}"))?,
            )),
            Self::Remote(sftp, path) => Ok(Box::new(
                sftp.open(path.as_str())
                    .await
                    .with_context(|| format!("read {self}"))?,
            )),
        }
    }

    pub(super) async fn remote_stage(&self) -> Result<RemoteFile> {
        let Self::Remote(sftp, path) = self else {
            bail!("expected a remote temporary path")
        };
        sftp.open_with_flags_and_attributes(
            path.as_str(),
            OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE,
            FileAttributes {
                permissions: Some(0o600),
                ..Default::default()
            },
        )
        .await
        .with_context(|| format!("create {self}"))
    }

    pub(super) async fn rename_to(&self, target: &Self) -> Result<()> {
        match (self, target) {
            (Self::Local(from), Self::Local(to)) => {
                if from != to {
                    let (from, to) = (from.clone(), to.clone());
                    tokio::task::spawn_blocking(move || rename_local(&from, &to))
                        .await?
                        .with_context(|| format!("rename {self} to {target}"))?;
                }
            }
            (Self::Remote(sftp, from), Self::Remote(_, to)) => {
                if from != to {
                    target.ensure_absent().await?;
                    sftp.rename(from.as_str(), to.as_str())
                        .await
                        .with_context(|| format!("rename {self} to {target}"))?;
                }
            }
            _ => bail!("rename requires paths on the same computer"),
        }
        Ok(())
    }

    pub(super) async fn remove_one(&self, kind: Kind) -> Result<()> {
        match self {
            Self::Local(path) => {
                #[cfg(windows)]
                let kind = if kind == Kind::Other {
                    use std::os::windows::fs::FileTypeExt;
                    if fs::symlink_metadata(path)
                        .await?
                        .file_type()
                        .is_symlink_dir()
                    {
                        Kind::Directory
                    } else {
                        kind
                    }
                } else {
                    kind
                };
                let result = if kind == Kind::Directory {
                    fs::remove_dir(path).await
                } else {
                    fs::remove_file(path).await
                };
                result.with_context(|| format!("delete {self}"))?;
            }
            Self::Remote(sftp, path) => {
                let result = if kind == Kind::Directory {
                    sftp.remove_dir(path.as_str()).await
                } else {
                    sftp.remove_file(path.as_str()).await
                };
                result.with_context(|| format!("delete {self}"))?;
            }
        }
        Ok(())
    }

    pub(super) async fn remove_tree(&self, depth: usize) -> Result<()> {
        let kind = self
            .kind()
            .await?
            .with_context(|| format!("{self} no longer exists"))?;
        if kind == Kind::Directory {
            if let Self::Local(path) = self {
                return fs::remove_dir_all(path)
                    .await
                    .with_context(|| format!("delete folder {self}"));
            }
            ensure!(
                depth < 128,
                "nested folders exceed the deletion depth limit"
            );
            for name in self.entries().await? {
                Box::pin(self.child(&name)?.remove_tree(depth + 1)).await?;
            }
        }
        self.remove_one(kind).await
    }

    async fn hash_file(&self) -> Result<blake3::Hash> {
        self.expect_file().await?;
        let mut input = self.reader().await?;
        let mut digest = blake3::Hasher::new();
        let mut buffer = vec![0; 256 * 1024];
        loop {
            let len = input.read(&mut buffer).await?;
            if len == 0 {
                return Ok(digest.finalize());
            }
            digest.update(&buffer[..len]);
        }
    }
}

// The local rename must reject an existing destination even if it appears after a check.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn rename_local(from: &Path, to: &Path) -> io::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let from = CString::new(from.as_os_str().as_bytes())?;
    let to = CString::new(to.as_os_str().as_bytes())?;
    #[cfg(target_os = "linux")]
    let status = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(target_os = "macos")]
    let status = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
    if status == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(all(windows, feature = "windows-client"))]
fn rename_local(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::MoveFileExW;
    fn extended(path: &Path) -> io::Result<Vec<u16>> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let full = std::fs::canonicalize(parent)?
            .join(path.file_name().ok_or(io::ErrorKind::InvalidInput)?);
        Ok(full.as_os_str().encode_wide().chain([0]).collect())
    }
    let (from, to) = (extended(from)?, extended(to)?);
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0) } != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    all(windows, feature = "windows-client")
)))]
fn rename_local(_from: &Path, _to: &Path) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

struct MoveEntry<'a> {
    source: Location<'a>,
    digest: Option<blake3::Hash>,
}

pub(super) struct Copier<P> {
    recursive: bool,
    pub(super) files: u64,
    progress: P,
}

impl<P: FnMut(Progress)> Copier<P> {
    pub(super) fn new(recursive: bool, progress: P) -> Self {
        Self {
            recursive,
            files: 0,
            progress,
        }
    }

    pub(super) async fn item(
        &mut self,
        source: &Location<'_>,
        target: &Location<'_>,
        moving: bool,
    ) -> Result<()> {
        ensure!(
            !source.contains(target),
            "select a different destination outside {source}"
        );
        target.ensure_absent().await?;
        let (kind, _) = source
            .details()
            .await?
            .with_context(|| format!("{source} no longer exists"))?;
        if moving {
            match (source, target) {
                (Location::Local(_), Location::Local(_)) => match source.rename_to(target).await {
                    Ok(()) => return Ok(()),
                    Err(error)
                        if error
                            .downcast_ref::<io::Error>()
                            .is_some_and(|e| e.kind() == io::ErrorKind::CrossesDevices) => {}
                    Err(error) => return Err(error),
                },
                (Location::Remote(..), Location::Remote(..)) => {
                    match source.rename_to(target).await {
                        Ok(()) => return Ok(()),
                        Err(error)
                            if target.kind().await?.is_none()
                                && source.kind().await? == Some(kind)
                                && kind != Kind::Other =>
                        {
                            // SFTP v3 usually reports a generic failure for moves across filesystems.
                            drop(error);
                        }
                        Err(error) => return Err(error),
                    }
                }
                _ => {}
            }
        }
        let mut entries = Vec::new();
        if kind == Kind::Directory {
            ensure!(self.recursive, "use -r to copy a folder");
            let staging = target.temporary("part")?;
            staging.create_dir().await?;
            let result = async {
                self.folder(source, &staging, 0, moving.then_some(&mut entries))
                    .await?;
                staging.rename_to(target).await
            }
            .await;
            if result.is_err() {
                let _ = staging.remove_tree(0).await;
            }
            result?;
        } else {
            self.file(source, target, moving.then_some(&mut entries))
                .await?;
        }
        if moving {
            // Verify the copied contents before deleting any source path. Remove only
            // the entries we copied, so a new child cannot be deleted by a recursive remove.
            for entry in &entries {
                let source = &entry.source;
                if let Some(digest) = entry.digest {
                    ensure!(
                        source.hash_file().await? == digest,
                        "{source} changed during the move; the copy was kept"
                    );
                } else {
                    ensure!(
                        source.kind().await? == Some(Kind::Directory),
                        "{source} changed during the move; the copy was kept"
                    );
                }
            }
            for entry in entries {
                let kind = if entry.digest.is_some() {
                    Kind::File
                } else {
                    Kind::Directory
                };
                entry
                    .source
                    .remove_one(kind)
                    .await
                    .context("the copy is ready, but the source could not be fully removed")?;
            }
        }
        Ok(())
    }

    async fn folder<'a>(
        &mut self,
        source: &Location<'a>,
        target: &Location<'_>,
        depth: usize,
        mut entries: Option<&mut Vec<MoveEntry<'a>>>,
    ) -> Result<()> {
        ensure!(
            depth < 128,
            "nested folders exceed the transfer depth limit"
        );
        for name in source.entries().await? {
            let (from, to) = (source.child(&name)?, target.child(&name)?);
            if from.kind().await? == Some(Kind::Directory) {
                to.create_dir().await?;
                Box::pin(self.folder(&from, &to, depth + 1, entries.as_deref_mut())).await?;
            } else {
                self.file(&from, &to, entries.as_deref_mut()).await?;
            }
        }
        if let Some(entries) = entries {
            entries.push(MoveEntry {
                source: source.clone(),
                digest: None,
            });
        }
        Ok(())
    }

    async fn file<'a>(
        &mut self,
        source: &Location<'a>,
        target: &Location<'_>,
        entries: Option<&mut Vec<MoveEntry<'a>>>,
    ) -> Result<()> {
        let (kind, size) = source
            .details()
            .await?
            .with_context(|| format!("{source} no longer exists"))?;
        ensure!(
            kind == Kind::File,
            "only regular files and folders can be copied: {source}"
        );
        let mut input = source.reader().await?;
        let name = source.name();
        let mut hash = entries.as_ref().map(|_| blake3::Hasher::new());
        match target {
            Location::Local(path) => {
                let Location::Local(parent) = target.parent() else {
                    unreachable!()
                };
                let stage = tempfile::Builder::new()
                    .prefix(".sshi-")
                    .suffix(".part")
                    .tempfile_in(parent)?;
                let mut output = fs::File::from_std(stage.reopen()?);
                transfer_data(
                    &mut input,
                    &mut output,
                    &name,
                    size,
                    &mut self.progress,
                    hash.as_mut(),
                )
                .await?;
                output.shutdown().await?;
                drop(output);
                stage
                    .persist_noclobber(path)
                    .with_context(|| format!("publish {target}"))?;
            }
            Location::Remote(..) => {
                let staging = target.temporary("part")?;
                let mut output = staging.remote_stage().await?;
                let result = async {
                    transfer_data(
                        &mut input,
                        &mut output,
                        &name,
                        size,
                        &mut self.progress,
                        hash.as_mut(),
                    )
                    .await?;
                    output
                        .close()
                        .await
                        .context("finish writing the remote file")?;
                    staging.rename_to(target).await
                }
                .await;
                if result.is_err() {
                    let _ = staging.remove_one(Kind::File).await;
                }
                result?;
            }
        }
        if let Some((entries, hash)) = entries.zip(hash) {
            entries.push(MoveEntry {
                source: source.clone(),
                digest: Some(hash.finalize()),
            });
        }
        self.files += 1;
        Ok(())
    }
}
