use std::net::{Ipv4Addr, Ipv6Addr};

use anyhow::{Context, Result, ensure};
use clap::ValueEnum;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ForwardKind {
    Local,
    Remote,
    Dynamic,
}

/// Validated TCP forwarding arguments for a single OpenSSH process.
pub struct ForwardSpec {
    flag: &'static str,
    rule: String,
}

impl ForwardSpec {
    pub fn new(
        kind: ForwardKind,
        bind_host: &str,
        bind_port: &str,
        target_host: &str,
        target_port: &str,
    ) -> Result<Self> {
        let listen = socket(bind_host, bind_port).context("invalid listen address or port")?;
        let flag = match kind {
            ForwardKind::Local => "-L",
            ForwardKind::Remote => "-R",
            ForwardKind::Dynamic => "-D",
        };
        let rule = if kind == ForwardKind::Dynamic {
            listen
        } else {
            let target =
                socket(target_host, target_port).context("invalid service address or port")?;
            format!("{listen}:{target}")
        };
        Ok(Self { flag, rule })
    }

    pub fn ssh_args(&self) -> [&str; 2] {
        [self.flag, &self.rule]
    }
}

fn socket(host: &str, port: &str) -> Result<String> {
    let port = port.trim();
    ensure!(
        port.bytes().all(|c| c.is_ascii_digit()) && port.parse::<u16>().is_ok_and(|p| p > 0),
        "port must be a number from 1 to 65535"
    );
    let host = host.trim();
    let unbracketed = host
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(host);
    if unbracketed.parse::<Ipv6Addr>().is_ok() {
        return Ok(format!("[{unbracketed}]:{port}"));
    }
    let dns = host.strip_suffix('.').unwrap_or(host);
    if dns.split('.').count() == 4 && dns.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        ensure!(dns.parse::<Ipv4Addr>().is_ok(), "invalid IPv4 address");
    }
    ensure!(
        dns.len() <= 253
            && dns.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            }),
        "expected an IP address or hostname"
    );
    Ok(format!("{host}:{port}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_each_route_for_openssh() {
        for (kind, flag) in [(ForwardKind::Local, "-L"), (ForwardKind::Remote, "-R")] {
            let route = ForwardSpec::new(kind, "::1", "8080", "[2001:db8::2]", "80").unwrap();
            assert_eq!(route.ssh_args(), [flag, "[::1]:8080:[2001:db8::2]:80"]);
        }
        let route = ForwardSpec::new(ForwardKind::Dynamic, "localhost", "1080", "", "").unwrap();
        assert_eq!(route.ssh_args(), ["-D", "localhost:1080"]);
        assert_eq!(socket("api.local.", "65535").unwrap(), "api.local.:65535");
    }

    #[test]
    fn rejects_ambiguous_or_invalid_fields() {
        for host in [
            "",
            "-host",
            "[example]",
            "http://localhost",
            "host:22",
            "a b",
            "a..b",
            "999.12.10.7",
        ] {
            assert!(socket(host, "80").is_err(), "{host}");
        }
        for port in ["", "0", "65536", "-1", "+1", "80:443"] {
            assert!(socket("localhost", port).is_err(), "{port}");
        }
    }
}
