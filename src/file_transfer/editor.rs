use anyhow::{Context, Result, ensure};
use russh_sftp::protocol::FileAttributes;
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncWriteExt},
};

use super::{Event, Kind, operations::Location};

const LIMIT: usize = 2 * 1024 * 1024;
const BOM: &[u8] = b"\xef\xbb\xbf";

struct Document {
    original: Vec<u8>,
    text: String,
    line_ending: &'static str,
    revision: String,
}

impl Document {
    async fn open(path: &Location<'_>) -> Result<Self> {
        path.expect_file().await?;
        let mut original = Vec::new();
        path.reader()
            .await?
            .take((LIMIT + 1) as u64)
            .read_to_end(&mut original)
            .await?;
        ensure!(
            original.len() <= LIMIT,
            "the text editor supports files up to 2 MiB"
        );
        let text = std::str::from_utf8(original.strip_prefix(BOM).unwrap_or(&original)).context(
            "the text editor can open UTF-8 files; download this file to open it in another app",
        )?;
        ensure!(
            !text.contains('\0'),
            "this file contains binary data; download it to open it in another app"
        );
        let line_ending = if text.contains("\r\n")
            && text.matches("\r\n").count() == text.matches('\n').count()
        {
            "\r\n"
        } else if !text.contains('\n') && text.contains('\r') {
            "\r"
        } else {
            "\n"
        };
        let text = normalize(text);
        let revision = blake3::hash(&original).to_hex().to_string();
        Ok(Self {
            original,
            text,
            line_ending,
            revision,
        })
    }

    fn encode(&self, text: &str) -> Result<Vec<u8>> {
        ensure!(
            text.len() <= LIMIT,
            "the text editor supports files up to 2 MiB"
        );
        ensure!(!text.contains('\0'), "the text contains a NUL character");
        let body = text.replace('\n', self.line_ending);
        let mut bytes = Vec::new();
        if self.original.starts_with(BOM) {
            bytes.extend_from_slice(BOM);
        }
        bytes.extend_from_slice(body.as_bytes());
        ensure!(
            bytes.len() <= LIMIT,
            "the text editor supports files up to 2 MiB"
        );
        Ok(bytes)
    }
}

fn normalize(value: &str) -> String {
    value.replace("\r\n", "\n").replace('\r', "\n")
}

async fn confirm_revision(path: &Location<'_>, expected: &str) -> Result<Document> {
    let current = Document::open(path)
        .await
        .context("check the file before saving")?;
    ensure!(
        current.revision == expected,
        "the file changed after it was opened; keep your edits and reopen the file before saving again"
    );
    Ok(current)
}

pub(super) async fn read(path: &Location<'_>) -> Result<Event> {
    let document = Document::open(path).await?;
    Ok(Event::Text {
        text: document.text,
        revision: document.revision,
    })
}

pub(super) async fn save(path: &Location<'_>, revision: &str, text: &str) -> Result<Event> {
    ensure!(
        text.len() <= LIMIT,
        "the text editor supports files up to 2 MiB"
    );
    let text = normalize(text);
    let original = confirm_revision(path, revision).await?;
    if text == original.text {
        return Ok(Event::Saved {
            revision: original.revision,
            warning: None,
        });
    }
    let bytes = original.encode(&text)?;
    let warning = match path {
        Location::Local(file) => {
            let Location::Local(parent) = path.parent() else {
                unreachable!()
            };
            let attributes = fs::symlink_metadata(file).await?;
            ensure!(attributes.is_file(), "expected a regular file at {path}");
            let temporary = tempfile::Builder::new()
                .prefix(".sshi-")
                .suffix(".part")
                .tempfile_in(parent)?;
            let mut output = fs::File::from_std(temporary.reopen()?);
            output.write_all(&bytes).await?;
            output.sync_all().await?;
            drop(output);
            preserve_local_owner(&temporary, &attributes)?;
            temporary
                .as_file()
                .set_permissions(attributes.permissions())?;
            confirm_revision(path, revision).await?;
            temporary
                .persist(file)
                .with_context(|| format!("save {path}"))?;
            None
        }
        Location::Remote(sftp, file) => {
            let original = sftp.symlink_metadata(file.as_str()).await?;
            ensure!(
                original.file_type().is_file(),
                "expected a regular file at {path}"
            );
            save_remote(path, original, revision, &bytes).await?
        }
    };
    Ok(Event::Saved {
        revision: blake3::hash(&bytes).to_hex().to_string(),
        warning,
    })
}

fn preserve_local_owner(
    stage: &tempfile::NamedTempFile,
    original: &std::fs::Metadata,
) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::{fd::AsRawFd, unix::fs::MetadataExt};
        let current = stage.as_file().metadata()?;
        if (current.uid(), current.gid()) != (original.uid(), original.gid()) {
            let result = unsafe {
                libc::fchown(stage.as_file().as_raw_fd(), original.uid(), original.gid())
            };
            if result != 0 {
                return Err(std::io::Error::last_os_error())
                    .context("preserve the file owner and group");
            }
        }
    }
    #[cfg(not(unix))]
    let _ = (stage, original);
    Ok(())
}

async fn save_remote(
    path: &Location<'_>,
    original: FileAttributes,
    revision: &str,
    bytes: &[u8],
) -> Result<Option<String>> {
    let stage = path.temporary("part")?;
    let backup = path.temporary("backup")?;
    let mut output = stage.remote_stage().await?;
    let result = async {
        output.write_all(bytes).await?;
        output.flush().await?;
        let current = output.metadata().await?;
        let (uid, gid) = if original.uid.zip(original.gid).is_some()
            && (original.uid, original.gid) != (current.uid, current.gid)
        {
            (original.uid, original.gid)
        } else {
            (None, None)
        };
        output
            .set_metadata(FileAttributes {
                uid,
                gid,
                permissions: original.permissions.map(|mode| mode & 0o7777),
                ..Default::default()
            })
            .await
            .context("preserve the remote file permissions and owner")?;
        output.sync_all().await?;
        output
            .close()
            .await
            .context("finish saving the temporary file")?;
        confirm_revision(path, revision).await?;
        // Standard SFTP rename publishes without overwriting. Keep the original
        // under a recoverable name while replacing it, even if the link drops.
        path.rename_to(&backup)
            .await
            .with_context(|| format!("back up the original file to {backup}"))?;
        if let Err(error) = stage.rename_to(path).await {
            return match backup.rename_to(path).await {
                Ok(()) => Err(error).context("the original file was restored"),
                Err(_) => Err(error)
                    .with_context(|| format!("the original file is preserved at {backup}")),
            };
        }
        let warning = backup
            .remove_one(Kind::File)
            .await
            .err()
            .map(|_| format!("saved; the original backup remains at {backup}"));
        Ok(warning)
    }
    .await;
    if result.is_err() {
        let _ = stage.remove_one(Kind::File).await;
    }
    result
}
