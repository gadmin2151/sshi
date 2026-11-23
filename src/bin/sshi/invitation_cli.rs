use std::io::{self, BufRead, IsTerminal, Read};

use anyhow::{Context, Result, ensure};
use clap::Subcommand;
use iroh::{EndpointId, RelayUrl};
use iroh_ssh::invite::{Code, InvitationRoute, MAX_INVITATION_BYTES, SharedInvitation};

use crate::{config::Config, profiles::PROFILE_RELAY, proxy, state::State};

#[derive(Subcommand)]
pub enum Action {
    /// Show only the public connection settings from a shared link as JSON.
    Inspect,
    /// Redeem a link or older invitation code for this computer's identity.
    Redeem {
        /// Receiver's Endpoint ID. Required for a code without a link.
        #[arg(long)]
        endpoint: Option<EndpointId>,
        /// Relay to use for an older code or link that omits the relay.
        #[arg(long)]
        relay: Option<String>,
    },
}

pub fn share_route(
    endpoint: EndpointId,
    user: &str,
    relay: Option<&str>,
) -> Result<InvitationRoute> {
    let url = match relay {
        Some(url) => url.to_owned(),
        None => Config::load()?
            .default_relay()?
            .context("no relay configured; use sshi config set-relay URL")?,
    };
    InvitationRoute::new(endpoint, user, Some(&url))
}

pub fn issue(client: EndpointId, user: Option<&str>, relay: Option<&str>) -> Result<()> {
    let state = State::load()?;
    // Fully validate the destination before replacing any previous invitation.
    let route = user
        .map(|name| {
            let profile_relay = iroh_ssh::env_value(PROFILE_RELAY)?;
            share_route(state.id()?, name, relay.or(profile_relay.as_deref()))
        })
        .transpose()?;
    let code = state.invitations()?.issue(client)?;
    match route {
        Some(route) => println!("{}", SharedInvitation { route, code }),
        None => println!("{code}"),
    }
    Ok(())
}

pub fn read_input() -> Result<String> {
    read_stdin(false)
}

fn read_stdin(whole_input: bool) -> Result<String> {
    let stdin = io::stdin();
    let text = if stdin.is_terminal() {
        rpassword::prompt_password("Iroh invitation: ")?
    } else {
        let mut text = String::new();
        let mut reader = stdin.lock().take(MAX_INVITATION_BYTES as u64 + 3);
        if whole_input {
            reader.read_to_string(&mut text)?;
        } else {
            reader.read_line(&mut text)?;
        }
        text
    };
    ensure!(
        text.len() <= MAX_INVITATION_BYTES + 2,
        "invitation is too long"
    );
    let text = text.trim_end_matches(['\r', '\n']);
    ensure!(text.len() <= MAX_INVITATION_BYTES, "invitation is too long");
    Ok(text.to_owned())
}

pub fn destination(
    text: &str,
    expected: Option<EndpointId>,
    expected_user: Option<&str>,
    relay: Option<RelayUrl>,
) -> Result<(EndpointId, Option<RelayUrl>, Code)> {
    let text = text.trim();
    if text.starts_with("sshi:") {
        let invite: SharedInvitation = text.parse()?;
        ensure!(
            expected_user.is_none_or(|user| user == invite.route.user),
            "SSH username differs from the invitation"
        );
        ensure!(
            expected.is_none_or(|id| id == invite.route.endpoint),
            "receiver Endpoint ID differs from the invitation"
        );
        if let (Some(explicit), Some(included)) = (&relay, &invite.route.relay) {
            ensure!(
                explicit == included,
                "relay URL differs from the invitation"
            );
        }
        Ok((
            invite.route.endpoint,
            invite.route.relay.or(relay),
            invite.code,
        ))
    } else {
        let code: Code = text.parse()?;
        Ok((
            expected.context(
                "a raw invitation code requires the receiver's Endpoint ID (--endpoint)",
            )?,
            relay,
            code,
        ))
    }
}

pub fn redeem(text: &str, expected: Option<EndpointId>, relay: Option<RelayUrl>) -> Result<()> {
    let (endpoint, relay, code) = destination(text, expected, None, relay)?;
    iroh_ssh::run_async(proxy::redeem(
        State::load()?,
        endpoint,
        code,
        relay.as_ref(),
    ))?;
    println!("Invitation accepted for this computer");
    Ok(())
}

pub fn run(action: Action) -> Result<()> {
    let input = read_stdin(true)?;
    match action {
        Action::Inspect => {
            let route = input.parse::<SharedInvitation>()?.route;
            println!(
                "{}",
                serde_json::json!({
                    "endpoint": route.endpoint.to_string(),
                    "user": route.user,
                    "relay": route.relay.as_ref().map(|url| url.as_str()).unwrap_or_default(),
                })
            );
            Ok(())
        }
        Action::Redeem { endpoint, relay } => redeem(
            &input,
            endpoint,
            relay
                .as_deref()
                .map(InvitationRoute::relay_url)
                .transpose()?,
        ),
    }
}
