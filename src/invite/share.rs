use std::{fmt, str::FromStr};

use anyhow::{Context, Result, anyhow, ensure};
use iroh::{EndpointId, RelayUrl};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};

use super::Code;

pub const MAX_INVITATION_BYTES: usize = 8192;
const PREFIX: &str = "sshi://invite/v1?";
const COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// Settings that may be shown before redeeming the invitation.
pub struct InvitationRoute {
    pub endpoint: EndpointId,
    pub user: String,
    pub relay: Option<RelayUrl>,
}

impl InvitationRoute {
    pub fn new(endpoint: EndpointId, user: &str, relay: Option<&str>) -> Result<Self> {
        ensure!(crate::profiles::valid_user(user), "invalid SSH username");
        let relay = relay.map(Self::relay_url).transpose()?;
        Ok(Self {
            endpoint,
            user: user.into(),
            relay,
        })
    }

    pub fn relay_url(text: &str) -> Result<RelayUrl> {
        ensure!(
            text.len() <= 2048 && !text.chars().any(|c| c.is_control() || c.is_whitespace()),
            "invalid invitation relay URL"
        );
        let url = crate::relay_url(text).map_err(|_| anyhow!("invalid invitation relay URL"))?;
        ensure!(
            url.query().is_none() && url.fragment().is_none() && url.as_str().len() <= 2048,
            "invalid invitation relay URL"
        );
        Ok(url)
    }
}

/// A shareable grant. Never include the code or full URL in error messages.
pub struct SharedInvitation {
    pub route: InvitationRoute,
    pub code: Code,
}

impl fmt::Display for SharedInvitation {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        let route = &self.route;
        write!(
            out,
            "{PREFIX}endpoint={}&user={}",
            route.endpoint,
            utf8_percent_encode(&route.user, COMPONENT)
        )?;
        if let Some(relay) = &route.relay {
            write!(
                out,
                "&relay={}",
                utf8_percent_encode(relay.as_str(), COMPONENT)
            )?;
        }
        write!(out, "#{}", self.code)
    }
}

impl FromStr for SharedInvitation {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        ensure!(text.len() <= MAX_INVITATION_BYTES, "invitation is too long");
        let body = text
            .trim()
            .strip_prefix(PREFIX)
            .context("expected an SSHI v1 invitation link")?;
        ensure!(
            body.is_ascii()
                && !body
                    .bytes()
                    .any(|b| b.is_ascii_control() || b.is_ascii_whitespace()),
            "invalid invitation link"
        );
        let (query, code) = body.split_once('#').context("missing invitation code")?;
        let code: Code = code.parse()?;
        let (mut endpoint, mut user, mut relay) = (None, None, None);
        for field in query.split('&') {
            let (name, value) = field.split_once('=').context("invalid invitation field")?;
            let dest = match name {
                "endpoint" => &mut endpoint,
                "user" => &mut user,
                "relay" => &mut relay,
                _ => anyhow::bail!("unknown invitation field"),
            };
            ensure!(dest.is_none(), "duplicate invitation field");
            let mut bytes = value.bytes();
            while let Some(b) = bytes.next() {
                if b == b'%' {
                    ensure!(
                        bytes.next().is_some_and(|b| b.is_ascii_hexdigit())
                            && bytes.next().is_some_and(|b| b.is_ascii_hexdigit()),
                        "invalid invitation field encoding"
                    );
                }
            }
            *dest = Some(
                percent_decode_str(value)
                    .decode_utf8()
                    .map_err(|_| anyhow!("invalid invitation field encoding"))?,
            );
        }
        let endpoint = endpoint
            .context("missing receiver Endpoint ID")?
            .parse::<EndpointId>()
            .map_err(|_| anyhow!("invalid receiver Endpoint ID"))?;
        let route = InvitationRoute::new(
            endpoint,
            &user.context("missing SSH username")?,
            relay.as_deref(),
        )?;
        Ok(Self { route, code })
    }
}
