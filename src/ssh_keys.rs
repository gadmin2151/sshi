pub fn install_ssh_key_command() -> String {
    format!(
        "if command -v flock >/dev/null 2>&1; then sh -c {} sh flock; else exec env PATH=\"$PATH:/usr/local/bin\" sshi __install-key; fi",
        crate::shell_arg(INSTALL_KEY)
    )
}

const INSTALL_KEY: &str = r#"
set -eu
umask 077
IFS= read -r key
info=$(printf '%s\n' "$key" | ssh-keygen -l -E sha256 -f -)
fingerprint=$(printf '%s\n' "$info" | awk 'NR == 1 { print $2 }')
test -n "$fingerprint"

dir="$HOME/.ssh"
file="$dir/authorized_keys"
mkdir -p "$dir"
exec 9>>"$file"
if [ "$1" = flock ]; then flock -x 9; fi

if ssh-keygen -l -E sha256 -f "$file" 2>/dev/null |
    awk -v wanted="$fingerprint" '$2 == wanted { found=1; exit } END { exit !found }'; then
    printf '%s\n' 'SSH public key is already installed.'
else
    test -z "$(tail -c 1 "$file")" || printf '\n' >&9
    printf '%s\n' "$key" >&9
    printf '%s\n' 'SSH public key installed.'
fi
"#;

#[cfg(unix)]
pub fn install_ssh_key_locally() -> anyhow::Result<()> {
    use anyhow::{Context, ensure};
    use std::{
        env,
        fs::{DirBuilder, OpenOptions},
        os::unix::fs::{DirBuilderExt, OpenOptionsExt},
        path::PathBuf,
        process::Command,
    };
    let dir = PathBuf::from(env::var_os("HOME").context("set HOME")?).join(".ssh");
    ensure!(dir.is_absolute(), "HOME must be absolute");
    DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    let file = OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(dir.join("authorized_keys"))?;
    file.lock().context("lock authorized_keys")?;
    let status = Command::new("sh")
        .args(["-c", INSTALL_KEY, "sh", "locked"])
        .status()?;
    ensure!(status.success(), "could not install SSH key ({status})");
    Ok(())
}
