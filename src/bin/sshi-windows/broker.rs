use crate::lang::{tr, tr_format};
use std::{
    env, fs,
    io::{self, Read, Write},
    net::Ipv4Addr,
    process::{Command, Stdio},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use iroh::EndpointId;
use iroh_ssh::{bind_relay, invite::Code, local, relay_url, run_async, tunnel::SharedTunnel};
use serde::{Deserialize, Serialize};
use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
    time::{sleep, timeout},
};

use crate::store::{Store, crypt};

const LOCATION: &str = "broker.dat";

#[derive(PartialEq, Serialize, Deserialize)]
pub struct Route {
    pub relay: String,
    pub token: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct BrokerLocation {
    port: u16,
    capability: [u8; 32],
    route: Route,
}

pub async fn connect(store: &Store, route: &Route, peer: EndpointId) -> Result<TcpStream> {
    let client = store.identity()?.public();
    let result = open_peer(store, route, client, peer).await;
    match result {
        Err(error) if local::was_closed(&error) => open_peer(store, route, client, peer).await,
        result => result,
    }
}

pub async fn redeem(store: &Store, route: &Route, peer: EndpointId, code: Code) -> Result<()> {
    let mut stream = open_authenticated(store, route).await?;
    let result = local::redeem(&mut stream, peer, &code).await;
    match result {
        Err(error) if local::was_closed(&error) => {
            let mut stream = open_authenticated(store, route).await?;
            local::redeem(&mut stream, peer, &code).await
        }
        result => result,
    }
}

async fn open_peer(
    store: &Store,
    route: &Route,
    client: EndpointId,
    peer: EndpointId,
) -> Result<TcpStream> {
    let mut stream = open_authenticated(store, route).await?;
    local::request(&mut stream, client, peer).await?;
    Ok(stream)
}

async fn open_authenticated(store: &Store, route: &Route) -> Result<TcpStream> {
    let (mut stream, capability) = open_local(store, route).await?;
    stream.write_all(&capability).await?;
    Ok(stream)
}

async fn open_local(store: &Store, route: &Route) -> Result<(TcpStream, [u8; 32])> {
    if let Some(connection) = running_broker(store, route).await? {
        return Ok(connection);
    }
    let _starting = store.lock("broker-start.lock", false)?;
    if let Some(connection) = running_broker(store, route).await? {
        return Ok(connection);
    }

    let mut process = Command::new(env::current_exe()?)
        .arg("--relay-broker")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context(tr(
            "Запустить общий туннель iroh",
            "Start the shared Iroh tunnel",
        ))?;
    let settings = toml::to_string(route)?;
    process
        .stdin
        .take()
        .context(tr(
            "Передать настройки общему туннелю iroh",
            "Send settings to the shared Iroh tunnel",
        ))?
        .write_all(settings.as_bytes())?;

    timeout(local::START_TIMEOUT, async {
        loop {
            if let Some(connection) = running_broker(store, route).await? {
                return Ok(connection);
            }
            if let Some(status) = process.try_wait()? {
                bail!(
                    "{}",
                    tr_format!(
                        "Процесс общего туннеля iroh завершился с кодом {status}",
                        "The shared Iroh tunnel exited with status {status}"
                    )
                );
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .context(tr(
        "Истекло время запуска общего туннеля iroh",
        "The shared Iroh tunnel timed out during startup",
    ))?
}

async fn running_broker(store: &Store, route: &Route) -> Result<Option<(TcpStream, [u8; 32])>> {
    let encrypted = match fs::read(store.path().join(LOCATION)) {
        Ok(data) => data,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).context(tr(
                "Открыть данные локального туннеля",
                "Open local tunnel state",
            ));
        }
    };
    let location: BrokerLocation = toml::from_slice(&crypt(&encrypted, false)?).context(tr(
        "Прочитать данные локального туннеля",
        "Read local tunnel state",
    ))?;
    let Ok(stream) = TcpStream::connect((Ipv4Addr::LOCALHOST, location.port)).await else {
        return Ok(None);
    };
    ensure!(
        location.route == *route,
        tr(
            "Общий туннель уже использует другой relay или токен. Закройте его окна перед сменой настроек.",
            "The shared tunnel is using a different relay or token. Close its windows before changing these settings."
        )
    );
    Ok(Some((stream, location.capability)))
}

pub fn run_child() -> Result<()> {
    let mut settings = String::new();
    io::stdin().read_to_string(&mut settings)?;
    let route = toml::from_str(&settings)?;
    run_async(serve(Store::open()?, route))
}

async fn serve(store: Store, route: Route) -> Result<()> {
    let lock = store.lock("connection.lock", true)?;
    let relay = relay_url(&route.relay)?;
    let endpoint = bind_relay(store.identity()?, &relay, route.token.clone()).await?;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let location = BrokerLocation {
        port: listener.local_addr()?.port(),
        capability: rand::random(),
        route,
    };
    // The anonymous loopback port is accessible to other local users. Require a
    // per-process capability protected by DPAPI before opening any iroh stream.
    let encoded = toml::to_string(&location)?;
    let protected = crypt(encoded.as_bytes(), true)?;
    let path = store.path().join(LOCATION);
    store.temp_file(&protected)?.persist(&path)?;

    let tunnel = Arc::new(SharedTunnel::new(endpoint.clone(), relay));
    let result = local::listen(
        async || Ok(listener.accept().await?.0),
        tunnel,
        Some(location.capability),
        Some(local::IDLE_TIMEOUT),
    )
    .await;
    let _closing = store.lock("broker-start.lock", false);
    drop(listener);
    let _ = fs::remove_file(path);
    endpoint.close().await;
    drop(lock);
    result
}
