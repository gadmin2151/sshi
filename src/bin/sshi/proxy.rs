use std::{
    collections::HashSet,
    env, fs,
    io::{ErrorKind, Write},
    net::SocketAddr,
    os::unix::{
        fs::{FileTypeExt, MetadataExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use iroh::{EndpointId, RelayUrl};
use iroh_ssh::{
    KeyInUse, bind_endpoint,
    invite::{self, Code},
    local,
    tunnel::{self, SharedTunnel},
};
use tempfile::NamedTempFile;
use tokio::{
    net::{UnixListener, UnixStream},
    time::{sleep, timeout},
};

use crate::{config::Config, state::State};

pub async fn connect(state: State, peer: EndpointId, invite_user: Option<&str>) -> Result<()> {
    let client = state.id()?;
    let (relay, token) = Config::load()?.relay()?;
    let token = token.as_deref();
    let opened = open_ssh(&state, client, peer, &relay, token).await;
    let stream = match (opened, invite_user) {
        (Err(error), Some(user))
            if error.is::<tunnel::ClientNotAllowed>()
                && crate::login_invite::enabled(user, peer) =>
        {
            let prompt = crate::login_invite::Prompt::new();
            let code = prompt.read(client, user, peer, &relay)?;
            redeem_on(&state, peer, code, &relay, token)
                .await
                .with_context(|| prompt.declined())?;
            eprintln!("{}", prompt.accepted());
            open_ssh(&state, client, peer, &relay, token).await?
        }
        (result, _) => result?,
    };
    let (recv, send) = stream.into_split();
    tunnel::proxy_stdio(recv, send).await
}

async fn open_ssh(
    state: &State,
    client: EndpointId,
    peer: EndpointId,
    relay: &RelayUrl,
    token: Option<&str>,
) -> Result<UnixStream> {
    let mut stream = open_local(state, relay, token).await?;
    if let Err(error) = local::request(&mut stream, client, peer).await {
        if !local::was_closed(&error) {
            return Err(error);
        }
        stream = open_local(state, relay, token).await?;
        local::request(&mut stream, client, peer).await?;
    }
    Ok(stream)
}

pub async fn redeem(
    state: State,
    peer: EndpointId,
    code: Code,
    relay: Option<&RelayUrl>,
) -> Result<()> {
    let (relay, token) = Config::load()?.relay_for(relay)?;
    state.id()?;
    redeem_on(&state, peer, code, &relay, token.as_deref()).await
}

async fn redeem_on(
    state: &State,
    peer: EndpointId,
    code: Code,
    relay: &RelayUrl,
    token: Option<&str>,
) -> Result<()> {
    let mut stream = open_local(state, relay, token).await?;
    let result = local::redeem(&mut stream, peer, &code).await;
    match result {
        Err(error) if local::was_closed(&error) => {
            let mut stream = open_local(state, relay, token).await?;
            local::redeem(&mut stream, peer, &code).await
        }
        result => result,
    }
}

async fn open_local(state: &State, relay: &RelayUrl, token: Option<&str>) -> Result<UnixStream> {
    let path = state.socket();
    let route = route_hash(relay, token);
    if let Some(stream) = local_socket(&path, &route).await? {
        return Ok(stream);
    }
    let _lock = state.broker_start_lock()?;
    if let Some(stream) = local_socket(&path, &route).await? {
        return Ok(stream);
    }
    let mut process = Command::new(env::current_exe()?)
        .env(crate::profiles::PROFILE_RELAY, relay.as_str())
        .arg("__broker")
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("start the local sshi broker")?;

    timeout(local::START_TIMEOUT, async {
        loop {
            if let Some(stream) = local_socket(&path, &route).await? {
                return Ok(stream);
            }
            if let Some(status) = process.try_wait()? {
                bail!("local sshi broker exited with {status}");
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .context("timed out starting the local sshi broker")?
}

fn route_hash(relay: &RelayUrl, token: Option<&str>) -> blake3::Hash {
    let mut hash = blake3::Hasher::new();
    hash.update(relay.as_str().as_bytes());
    hash.update(&[0]);
    hash.update(token.unwrap_or_default().as_bytes());
    hash.finalize()
}

fn route_marker(path: &Path, route: &blake3::Hash) -> Result<String> {
    let socket = fs::symlink_metadata(path)?;
    Ok(format!("{} {} {}\n", socket.dev(), socket.ino(), route))
}

async fn local_socket(path: &Path, route: &blake3::Hash) -> Result<Option<UnixStream>> {
    match UnixStream::connect(path).await {
        Ok(stream) => {
            let marker = route_marker(path, route)?;
            let file = path.with_extension("route");
            // bind() creates the socket before its inode can be recorded. Allow
            // the new owner to finish publishing the marker during startup.
            for attempt in 0..3 {
                if fs::read_to_string(&file).is_ok_and(|actual| actual == marker) {
                    return Ok(Some(stream));
                }
                if attempt < 2 {
                    sleep(Duration::from_millis(25)).await;
                }
            }
            bail!(
                "the active sshi service has different relay settings or an older version; restart the receiver service or close its existing client windows before reconnecting"
            );
        }
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::NotFound | ErrorKind::ConnectionRefused
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error).with_context(|| format!("connect to {}", path.display())),
    }
}

pub async fn broker(state: State) -> Result<()> {
    let (relay, token) = Config::load()?.relay()?;
    state.id()?;
    let route = route_hash(&relay, token.as_deref());
    let (endpoint, key_file) = bind_endpoint(&state.key(), &relay, token).await?;
    let (listener, cleanup) = bind_local(&state, &route)?;
    let shared = Arc::new(SharedTunnel::new(endpoint.clone(), relay));
    let result = local::listen(
        async || Ok(listener.accept().await?.0),
        shared,
        None,
        Some(local::IDLE_TIMEOUT),
    )
    .await;
    let _closing = state.broker_start_lock();
    drop(listener);
    drop(cleanup);
    // Keep the identity registered until the endpoint has finished closing.
    endpoint.close().await;
    drop(key_file);
    result
}

pub async fn serve(
    state: State,
    clients: Vec<EndpointId>,
    target: Option<SocketAddr>,
) -> Result<()> {
    let config = Config::load()?;
    let (relay, token) = config.relay()?;
    let target = match target {
        Some(target) => target,
        None => config.ssh_target()?,
    };
    let explicit: HashSet<_> = clients.into_iter().collect();
    let invitations = state.invitations()?;
    let handler = {
        let invitations = invitations.clone();
        invite::Receiver::new(move |client, code| invitations.redeem(client, code))
    };
    let allowed = move |client| {
        explicit.contains(&client)
            || match Config::load() {
                Ok(config) => {
                    config.allow_clients.contains(&client)
                        || match invitations.allows(client) {
                            Ok(allowed) => allowed,
                            Err(error) => {
                                eprintln!(
                                    "Could not read invitation access for {client}: {error:#}"
                                );
                                false
                            }
                        }
                }
                Err(error) => {
                    eprintln!("Could not read allow_clients: {error:#}");
                    false
                }
            }
    };
    let route = route_hash(&relay, token.as_deref());
    // The last outgoing session can finish just before its idle broker releases
    // this identity. Allow that handoff without disrupting any active windows.
    let deadline = tokio::time::Instant::now() + local::START_TIMEOUT;
    let (endpoint, _key_file) = loop {
        match bind_endpoint(&state.key(), &relay, token.clone()).await {
            Err(err) if err.is::<KeyInUse>() && tokio::time::Instant::now() < deadline => {
                sleep(Duration::from_millis(100)).await;
            }
            result => break result?,
        }
    };
    let (listener, _cleanup) = bind_local(&state, &route)?;
    let shared = Arc::new(SharedTunnel::new(endpoint.clone(), relay.clone()));
    tokio::select! {
        result = tunnel::serve_with_invites(endpoint, relay, allowed, target, Some(handler)) => result,
        result = local::listen(async || Ok(listener.accept().await?.0), shared, None, None) => result,
    }
}

fn bind_local(state: &State, route: &blake3::Hash) -> Result<(UnixListener, SocketCleanup)> {
    let path = state.socket();
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            ensure!(
                metadata.file_type().is_socket(),
                "{} is not a socket",
                path.display()
            );
            fs::remove_file(&path).context("remove stale sshi socket")?;
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("inspect sshi socket"),
    }
    let listener =
        UnixListener::bind(&path).with_context(|| format!("listen on {}", path.display()))?;
    let cleanup = SocketCleanup(path.clone());
    let mut marker = NamedTempFile::new_in(path.parent().context("locate broker directory")?)?;
    marker.write_all(route_marker(&path, route)?.as_bytes())?;
    marker.persist(path.with_extension("route"))?;
    Ok((listener, cleanup))
}

struct SocketCleanup(PathBuf);

impl Drop for SocketCleanup {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.0.with_extension("route"));
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn existing_socket_requires_the_matching_route_and_token() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("proxy.sock");
        let _listener = UnixListener::bind(&path)?;
        let relay: RelayUrl = "https://test.example/".parse()?;
        let original = route_hash(&relay, Some("first-fixture"));
        fs::write(
            path.with_extension("route"),
            route_marker(&path, &original)?,
        )?;
        assert!(local_socket(&path, &original).await?.is_some());
        let changed = route_hash(&relay, Some("other-fixture"));
        assert!(local_socket(&path, &changed).await.is_err());
        Ok(())
    }
}
