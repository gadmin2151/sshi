use std::{fs::File, net::SocketAddr, path::PathBuf};

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use iroh::{Endpoint, EndpointId, RelayUrl};
use iroh_ssh::{
    DEFAULT_SSH_TARGET, RELAY_TOKEN_ENV, RELAY_URL_ENV, bind_endpoint, env_value, generate_key,
    relay_url, run_async, ssh_target, tunnel,
};

#[derive(Parser)]
#[command(version, about = "Run OpenSSH over an iroh relay")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a private iroh key and print its public Endpoint ID.
    Keygen {
        #[arg(long)]
        key: PathBuf,
    },
    /// Expose a local SSH daemon to the listed iroh clients.
    Serve {
        #[command(flatten)]
        endpoint: EndpointArgs,
        /// Authorized iroh client Endpoint ID. Repeat this option for more clients.
        #[arg(long, required = true, value_delimiter = ',')]
        allow_client: Vec<EndpointId>,
        /// Address of sshd, on this machine's loopback interface.
        #[arg(long, default_value = DEFAULT_SSH_TARGET, value_parser = ssh_target)]
        target: SocketAddr,
    },
    /// Connect to an iroh-ssh server over stdin/stdout, for OpenSSH ProxyCommand.
    Connect {
        #[command(flatten)]
        endpoint: EndpointArgs,
        /// Endpoint ID printed by the server.
        #[arg(long, env = "IROH_RENDEZVOUS_ENDPOINT_ID")]
        server: EndpointId,
    },
}

#[derive(Args)]
struct EndpointArgs {
    /// Private iroh key created by keygen.
    #[arg(long)]
    key: PathBuf,
    /// Relay URL. Set IROH_RELAY_ACCESS_TOKEN for an authenticated relay.
    #[arg(long, env = RELAY_URL_ENV, value_parser = relay_url)]
    relay: RelayUrl,
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Keygen { key } => {
            println!("{}", generate_key(&key)?);
            Ok(())
        }
        Command::Serve {
            endpoint,
            allow_client,
            target,
        } => run_async(async move {
            let (node, _key_file) = endpoint.bind().await?;
            tunnel::serve(
                node,
                endpoint.relay,
                allow_client.into_iter().collect(),
                target,
            )
            .await
        }),
        Command::Connect { endpoint, server } => run_async(async move {
            let (node, _key_file) = endpoint.bind().await?;
            tunnel::connect(node, endpoint.relay, server).await
        }),
    }
}

impl EndpointArgs {
    async fn bind(&self) -> Result<(Endpoint, File)> {
        bind_endpoint(&self.key, &self.relay, env_value(RELAY_TOKEN_ENV)?).await
    }
}
