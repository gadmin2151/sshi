use std::{
    collections::{HashMap, HashSet},
    fmt,
    net::SocketAddr,
    sync::{Arc, Weak},
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use iroh::{
    Endpoint, EndpointAddr, EndpointId, RelayUrl, Watcher,
    endpoint::{
        ConnectError, ConnectOptions, ConnectingError, Connection, ConnectionError, RecvStream,
        SendStream,
    },
    protocol::{AcceptError, ProtocolHandler, Router},
};
use tokio::{
    io::{self, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    signal,
    sync::Mutex,
    task::JoinSet,
    time::timeout,
};

use crate::invite::{self, Code};

const ALPN: &[u8] = b"iroh-ssh/1";
const SHARED_ALPN: &[u8] = b"iroh-ssh/2";
const OPEN_SSH: u8 = 1;
const SSH_READY: u8 = 2;
const SSH_DENIED: u8 = 3;
const CLIENT_NOT_ALLOWED: u32 = 1;
const SETUP_TIMEOUT: Duration = Duration::from_secs(15);
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub struct ClientNotAllowed {
    pub client: EndpointId,
}

impl fmt::Display for ClientNotAllowed {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(out, "client Endpoint ID {} is not allowed", self.client)
    }
}

impl std::error::Error for ClientNotAllowed {}

pub async fn serve(
    endpoint: Endpoint,
    relay: RelayUrl,
    allowed: HashSet<EndpointId>,
    target: SocketAddr,
) -> Result<()> {
    serve_with_allowlist(
        endpoint,
        relay,
        move |client| allowed.contains(&client),
        target,
    )
    .await
}

pub async fn serve_with_allowlist(
    endpoint: Endpoint,
    relay: RelayUrl,
    allowed: impl Fn(EndpointId) -> bool + Send + Sync + 'static,
    target: SocketAddr,
) -> Result<()> {
    serve_with_invites(endpoint, relay, allowed, target, None).await
}

pub async fn serve_with_invites(
    endpoint: Endpoint,
    relay: RelayUrl,
    allowed: impl Fn(EndpointId) -> bool + Send + Sync + 'static,
    target: SocketAddr,
    invitations: Option<invite::Receiver>,
) -> Result<()> {
    let allowed = Arc::new(allowed);
    let router = Router::builder(endpoint.clone())
        .accept(
            SHARED_ALPN,
            SshBridge {
                allowed: allowed.clone(),
                target,
            },
        )
        .accept(ALPN, SshBridge { allowed, target });
    let router = match invitations {
        Some(handler) => router.accept(invite::ALPN, handler),
        None => router,
    }
    .spawn();
    let run = async {
        wait_for_relay(&endpoint, &relay).await?;
        eprintln!("Ready: {} via {relay} -> {target}", endpoint.id());
        endpoint.closed().await;
        bail!("iroh endpoint closed")
    };
    let result = tokio::select! {
        result = run => result,
        result = shutdown_signal() => result.context("listen for shutdown signal"),
    };
    let shutdown = router.shutdown().await.context("shut down the SSH server");
    result.and(shutdown)
}

pub async fn connect(endpoint: Endpoint, relay: RelayUrl, server: EndpointId) -> Result<()> {
    let run = async {
        let (_connection, send, recv) = dial(&endpoint, &relay, server).await?;
        proxy_stdio(recv, send).await
    };
    let result = run.await;
    endpoint.close().await;
    result
}

pub async fn dial(
    endpoint: &Endpoint,
    relay: &RelayUrl,
    server: EndpointId,
) -> Result<(Connection, SendStream, RecvStream)> {
    let connection = connect_peer(endpoint, relay, server, false).await?;
    let (send, recv) = ssh_stream(&connection, endpoint.id()).await?;
    Ok((connection, send, recv))
}

type PeerConnection = Arc<Mutex<Weak<Connection>>>;

pub struct SharedTunnel {
    endpoint: Endpoint,
    relay: RelayUrl,
    peers: Mutex<HashMap<EndpointId, PeerConnection>>,
}

impl SharedTunnel {
    pub fn new(endpoint: Endpoint, relay: RelayUrl) -> Self {
        Self {
            endpoint,
            relay,
            peers: Mutex::new(HashMap::new()),
        }
    }

    async fn connection(&self, peer: EndpointId) -> Result<Arc<Connection>> {
        let slot = {
            let mut peers = self.peers.lock().await;
            if !peers.contains_key(&peer) && peers.len().is_multiple_of(64) && !peers.is_empty() {
                // On long-running receivers, discard idle peer slots without interrupting
                // an active connection or another caller that is about to connect.
                peers.retain(|_, slot| {
                    Arc::strong_count(slot) > 1
                        || slot.try_lock().map_or(true, |conn| conn.strong_count() > 0)
                });
            }
            peers.entry(peer).or_default().clone()
        };
        let mut cached = slot.lock().await;
        if let Some(connection) = cached
            .upgrade()
            .filter(|conn| conn.close_reason().is_none())
        {
            return Ok(connection);
        }
        let connection = Arc::new(connect_peer(&self.endpoint, &self.relay, peer, true).await?);
        if connection.alpn() == SHARED_ALPN {
            *cached = Arc::downgrade(&connection);
        }
        Ok(connection)
    }

    pub async fn open(
        &self,
        peer: EndpointId,
    ) -> Result<(Arc<Connection>, SendStream, RecvStream)> {
        let mut connection = self.connection(peer).await?;
        let client = self.endpoint.id();
        let first = ssh_stream(&connection, client).await;
        let (send, recv) = match first {
            Err(ref error)
                if connection.alpn() == SHARED_ALPN
                    && connection.close_reason().is_some()
                    && !error.is::<ClientNotAllowed>() =>
            {
                connection = self.connection(peer).await?;
                ssh_stream(&connection, client).await?
            }
            result => result?,
        };
        Ok((connection, send, recv))
    }

    pub async fn redeem(&self, peer: EndpointId, code: Code) -> Result<()> {
        invite::redeem(&self.endpoint, &self.relay, peer, code).await
    }
}

async fn connect_peer(
    endpoint: &Endpoint,
    relay: &RelayUrl,
    server: EndpointId,
    shared: bool,
) -> Result<Connection> {
    wait_for_relay(endpoint, relay).await?;
    let addr = EndpointAddr::new(server).with_relay_url(relay.clone());
    let connect = async {
        if shared {
            let options = ConnectOptions::new().with_additional_alpns(vec![ALPN.to_vec()]);
            Ok(endpoint
                .connect_with_opts(addr, SHARED_ALPN, options)
                .await?
                .await?)
        } else {
            endpoint.connect(addr, ALPN).await
        }
    };
    let connecting = timeout(SETUP_TIMEOUT, connect)
        .await
        .with_context(|| format!("timed out connecting to {server} via {relay}"))?;
    match connecting {
        Err(
            ConnectError::Connection { source, .. }
            | ConnectError::Connecting {
                source: ConnectingError::ConnectionError { source, .. },
                ..
            },
        ) if access_denied(&source) => Err(ClientNotAllowed {
            client: endpoint.id(),
        }
        .into()),
        result => result.with_context(|| format!("connect to {server} via {relay}")),
    }
}

async fn ssh_stream(
    connection: &Connection,
    client: EndpointId,
) -> Result<(SendStream, RecvStream)> {
    timeout(SETUP_TIMEOUT, open_ssh(connection, client))
        .await
        .context("timed out opening the SSH stream")?
        .map_err(|error| {
            if connection
                .close_reason()
                .as_ref()
                .is_some_and(access_denied)
            {
                error.context(ClientNotAllowed { client })
            } else {
                error
            }
        })
}

fn access_denied(error: &ConnectionError) -> bool {
    matches!(error, ConnectionError::ApplicationClosed(close) if close.error_code == CLIENT_NOT_ALLOWED.into())
}

pub(crate) async fn wait_for_relay(endpoint: &Endpoint, relay: &RelayUrl) -> Result<()> {
    let mut watcher = endpoint.home_relay_status();
    let wait = async {
        let mut statuses = watcher.get();
        loop {
            for status in &statuses {
                if status.is_connected() {
                    return Ok(());
                }
                if status.auth_denied_reason().is_some() {
                    bail!("relay {relay} rejected authentication (check IROH_RELAY_ACCESS_TOKEN)");
                }
            }
            statuses = watcher.updated().await.context("watch relay status")?;
        }
    };
    match timeout(SETUP_TIMEOUT, wait).await {
        Ok(result) => result,
        Err(_) => {
            let statuses = watcher.get();
            if let Some(error) = statuses.iter().find_map(|status| status.last_error()) {
                bail!("could not connect to relay {relay}: {error:#}");
            }
            bail!("timed out connecting to relay {relay}");
        }
    }
}

struct SshBridge<Allow> {
    allowed: Arc<Allow>,
    target: SocketAddr,
}

impl<Allow> fmt::Debug for SshBridge<Allow> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SshBridge")
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

impl<Allow: Fn(EndpointId) -> bool + Send + Sync + 'static> ProtocolHandler for SshBridge<Allow> {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let client = connection.remote_id();
        if !(self.allowed)(client) {
            eprintln!("Denied iroh client {client}");
            connection.close(
                CLIENT_NOT_ALLOWED.into(),
                b"client Endpoint ID is not allowed",
            );
            return Ok(());
        }

        if connection.alpn() == SHARED_ALPN {
            eprintln!("Shared iroh connection from {client}");
            self.forward_streams(&connection, client).await;
        } else if let Err(error) = self.forward_one(&connection).await
            && connection.close_reason().is_none()
        {
            eprintln!("SSH connection from {client}: {error:#}");
            connection.close(
                2u32.into(),
                b"could not relay SSH; check the iroh-ssh server log",
            );
        }
        Ok(())
    }
}

impl<Allow: Fn(EndpointId) -> bool> SshBridge<Allow> {
    async fn forward_one(&self, connection: &Connection) -> Result<()> {
        let (send, recv) = timeout(SETUP_TIMEOUT, connection.accept_bi())
            .await
            .context("timed out waiting for the SSH stream")??;
        forward_stream(send, recv, self.target).await?;

        // Keep the connection alive while the client consumes the final bytes and FIN.
        let _ = timeout(DRAIN_TIMEOUT, connection.closed()).await;
        Ok(())
    }

    async fn forward_streams(&self, connection: &Connection, client: EndpointId) {
        let mut streams = JoinSet::new();
        loop {
            tokio::select! {
                stream = connection.accept_bi() => {
                    let Ok((mut send, recv)) = stream else { break };
                    if !(self.allowed)(client) {
                        eprintln!("Denied iroh client {client}");
                        let _ = send.write_all(&[SSH_DENIED]).await;
                        let _ = send.shutdown().await;
                        continue;
                    }
                    let target = self.target;
                    streams.spawn(async move {
                        if let Err(error) = forward_stream(send, recv, target).await {
                            eprintln!("SSH stream from {client}: {error:#}");
                        }
                    });
                }
                _ = streams.join_next(), if !streams.is_empty() => {}
            }
        }
    }
}

async fn forward_stream(
    mut send: SendStream,
    mut recv: RecvStream,
    target: SocketAddr,
) -> Result<()> {
    let setup = async {
        ensure!(
            recv.read_u8().await? == OPEN_SSH,
            "invalid SSH stream request"
        );
        let tcp = TcpStream::connect(target)
            .await
            .with_context(|| format!("connect to local sshd at {target}"))?;
        tcp.set_nodelay(true).context("configure SSH socket")?;
        send.write_all(&[SSH_READY])
            .await
            .context("confirm SSH stream")?;
        Ok::<_, anyhow::Error>(tcp)
    };
    let mut tcp = timeout(SETUP_TIMEOUT, setup)
        .await
        .context("SSH stream setup timed out")??;
    io::copy_bidirectional(&mut io::join(recv, send), &mut tcp)
        .await
        .context("forward SSH traffic")?;
    Ok(())
}

async fn open_ssh(connection: &Connection, client: EndpointId) -> Result<(SendStream, RecvStream)> {
    let (mut send, mut recv) = connection.open_bi().await.context("open QUIC stream")?;
    // A peer only sees a QUIC stream after bytes are sent. Open it before reading stdin.
    send.write_all(&[OPEN_SSH])
        .await
        .context("request SSH stream")?;
    match recv
        .read_u8()
        .await
        .context("wait for the receiver's sshd")?
    {
        SSH_READY => Ok((send, recv)),
        SSH_DENIED if connection.alpn() == SHARED_ALPN => Err(ClientNotAllowed { client }.into()),
        _ => bail!("invalid SSH stream response"),
    }
}

pub async fn proxy_stdio(
    mut recv: impl AsyncRead + Unpin,
    mut send: impl AsyncWrite + Unpin,
) -> Result<()> {
    let mut stdin = io::stdin();
    let mut stdout = io::stdout();
    let upload = async {
        io::copy(&mut stdin, &mut send)
            .await
            .context("send SSH input")?;
        send.shutdown().await.context("finish SSH input")
    };
    let download = io::copy(&mut recv, &mut stdout);
    tokio::pin!(download);

    // OpenSSH may leave stdin open until it sees EOF from the server.
    let output = tokio::select! {
        biased;
        result = &mut download => result,
        result = upload => {
            result?;
            download.await
        }
    };
    output.context("receive SSH output")?;
    Ok(())
}

async fn shutdown_signal() -> io::Result<()> {
    #[cfg(unix)]
    let mut terminate = signal::unix::signal(signal::unix::SignalKind::terminate())?;
    #[cfg(unix)]
    let terminate = terminate.recv();
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        result = signal::ctrl_c() => result,
        _ = terminate => Ok(()),
    }
}
