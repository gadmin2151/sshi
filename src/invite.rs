use std::{fmt, str::FromStr, sync::Arc, time::Duration};

use anyhow::{Context, Result, bail};
use iroh::{
    Endpoint, EndpointAddr, EndpointId, RelayUrl, SecretKey,
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler},
};
use tokio::{sync::Semaphore, task, time::timeout};

use crate::tunnel::wait_for_relay;

mod share;
pub use share::{InvitationRoute, MAX_INVITATION_BYTES, SharedInvitation};

pub(crate) const ALPN: &[u8] = b"iroh-ssh/enroll/1";
const TIMEOUT: Duration = Duration::from_secs(10);

pub struct Code([u8; 32]);

impl Code {
    pub fn generate() -> Self {
        Self(SecretKey::generate().to_bytes())
    }

    pub(crate) fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub(crate) fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn fingerprint(&self) -> blake3::Hash {
        blake3::hash(&self.0)
    }
}

impl FromStr for Code {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        let bytes = blake3::Hash::from_hex(text.trim())
            .map_err(|_| anyhow::anyhow!("expected a 64-character invitation code"))?;
        Ok(Self(*bytes.as_bytes()))
    }
}

impl fmt::Display for Code {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        blake3::Hash::from_bytes(self.0).fmt(out)
    }
}

pub(crate) async fn redeem(
    endpoint: &Endpoint,
    relay: &RelayUrl,
    peer: EndpointId,
    code: Code,
) -> Result<()> {
    wait_for_relay(endpoint, relay).await?;
    let addr = EndpointAddr::new(peer).with_relay_url(relay.clone());
    let connection = timeout(TIMEOUT, endpoint.connect(addr, ALPN))
        .await
        .context("timed out connecting to the invitation service")?
        .context("connect to the invitation service over iroh (receiver needs sshi 0.1.4+)")?;
    timeout(TIMEOUT, async {
        let (mut send, mut recv) = connection.open_bi().await?;
        send.write_all(code.as_bytes()).await?;
        send.finish()?;
        let mut status = [0];
        recv.read_exact(&mut status).await?;
        match status[0] {
            1 => Ok(()),
            0 => bail!(
                "invitation denied: verify the code, expiration and this computer's Endpoint ID"
            ),
            _ => bail!("invalid response to the invitation request"),
        }
    })
    .await
    .context("timed out waiting for the invitation response")?
}

type Redeem = dyn Fn(EndpointId, Code) -> Result<bool> + Send + Sync;

pub struct Receiver {
    redeem: Arc<Redeem>,
    in_flight: Semaphore,
}

impl Receiver {
    pub fn new(redeem: impl Fn(EndpointId, Code) -> Result<bool> + Send + Sync + 'static) -> Self {
        Self {
            redeem: Arc::new(redeem),
            in_flight: Semaphore::new(32),
        }
    }
}

impl fmt::Debug for Receiver {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("InvitationReceiver")
            .finish_non_exhaustive()
    }
}

impl ProtocolHandler for Receiver {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let _permit = self
            .in_flight
            .try_acquire()
            .map_err(AcceptError::from_err)?;
        timeout(TIMEOUT, async {
            let client = connection.remote_id();
            let (mut send, mut recv) = connection.accept_bi().await?;
            let mut bytes = [0; 32];
            recv.read_exact(&mut bytes).await?;
            let redeem = self.redeem.clone();
            let accepted = match task::spawn_blocking(move || redeem(client, Code(bytes))).await? {
                Ok(accepted) => accepted,
                Err(error) => {
                    eprintln!("Could not redeem sshi invitation for {client}: {error:#}");
                    false
                }
            };
            send.write_all(&[u8::from(accepted)]).await?;
            send.finish()?;
            let _ = send.stopped().await;
            Ok::<_, anyhow::Error>(())
        })
        .await
        .map_err(AcceptError::from_err)?
        .map_err(|error| AcceptError::from_boxed(error.into_boxed_dyn_error()))
    }
}
