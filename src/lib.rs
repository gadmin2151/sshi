pub mod file_transfer;
pub mod forwarding;
mod identity;
pub mod invite;
pub mod local;
pub mod profiles;
mod ssh_keys;
pub mod tunnel;
pub mod update;

use std::{env, fs::File, future::Future, net::SocketAddr, path::Path};

use anyhow::{Context, Result, bail, ensure};
use iroh::{Endpoint, EndpointId, RelayConfig, RelayMode, RelayUrl, SecretKey, endpoint::presets};

pub use identity::{KeyInUse, generate_key, key_id};
pub use ssh_keys::install_ssh_key_command;
#[cfg(unix)]
pub use ssh_keys::install_ssh_key_locally;

pub const RELAY_URL_ENV: &str = "IROH_RELAY_URL";
pub const RELAY_TOKEN_ENV: &str = "IROH_RELAY_ACCESS_TOKEN";
pub const DEFAULT_SSH_TARGET: &str = "127.0.0.1:22";

pub fn allow_client_command(client: EndpointId) -> String {
    format!(r#"PATH="$PATH:/usr/local/bin" sshi allow-client {client}"#)
}

pub fn shell_arg(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn run_async<T>(task: impl Future<Output = Result<T>>) -> Result<T> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("start async runtime")?;
    let result = runtime.block_on(task);
    // Tokio stdin uses a blocking read that may still be waiting on OpenSSH's pipe.
    runtime.shutdown_background();
    result
}

pub async fn bind_endpoint(
    key: &Path,
    relay: &RelayUrl,
    token: Option<String>,
) -> Result<(Endpoint, File)> {
    let (key, key_file) = identity::lock_key(key)?;
    let endpoint = bind_relay(key, relay, token).await?;
    Ok((endpoint, key_file))
}

pub async fn bind_relay(
    key: SecretKey,
    relay: &RelayUrl,
    token: Option<String>,
) -> Result<Endpoint> {
    // Peer addresses are supplied explicitly, so QUIC address discovery is unnecessary.
    let mut relay_config = RelayConfig::new(relay.clone(), None);
    if let Some(token) = token {
        ensure!(!token.trim().is_empty(), "the relay token is empty");
        ensure!(
            relay.scheme() == "https",
            "an authenticated relay requires HTTPS"
        );
        relay_config = relay_config.with_auth_token(token);
    }

    Endpoint::builder(presets::Minimal)
        .secret_key(key)
        .clear_ip_transports()
        .relay_mode(RelayMode::Custom(relay_config.into()))
        .bind()
        .await
        .context("bind iroh endpoint")
}

pub fn env_value(name: &str) -> Result<Option<String>> {
    match env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => bail!("{name} must be valid UTF-8"),
    }
}

pub fn ssh_target(value: &str) -> Result<SocketAddr> {
    let addr: SocketAddr = value.parse().context("expected an IP address and port")?;
    ensure!(
        addr.ip().is_loopback() && addr.port() != 0,
        "expected a loopback address with a nonzero port"
    );
    Ok(addr)
}

pub fn relay_url(value: &str) -> Result<RelayUrl> {
    let url: RelayUrl = value.parse()?;
    ensure!(
        matches!(url.scheme(), "http" | "https") && url.has_host(),
        "expected an HTTP or HTTPS relay URL"
    );
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "put relay credentials in {RELAY_TOKEN_ENV} or a separate token file"
    );
    Ok(url)
}

/// Issue the grant only under the account that owns the selected receiver identity.
pub fn issue_invitation_command(
    receiver: EndpointId,
    client: EndpointId,
    service_user: Option<&str>,
) -> Result<String> {
    let script = r#"set -eu
PATH="$PATH:/usr/local/bin"
export PATH
case "${XDG_STATE_HOME:-}" in
    /*) state_home=$XDG_STATE_HOME ;;
    *) state_home=$HOME/.local/state ;;
esac
if [ ! -f "$state_home/sshi/endpoint.key" ] || [ "$(sshi id)" != "$1" ]; then
    printf '%s\n' 'The service account does not own the selected receiver Endpoint ID.' >&2
    exit 1
fi
exec sshi invite "$2""#;
    let command = format!("sh -c {} sh {receiver} {client}", shell_arg(script));
    if let Some(user) = service_user {
        ensure!(
            profiles::valid_user(user),
            "invalid receiver service username"
        );
        let user = shell_arg(user);
        Ok(format!(
            "if command -v runuser >/dev/null 2>&1; then exec runuser -u {user} -- {command}; else exec sudo -n -H -u {user} -- {command}; fi"
        ))
    } else {
        Ok(command)
    }
}
