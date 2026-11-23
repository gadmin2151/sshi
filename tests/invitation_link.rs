use anyhow::Result;
use iroh::SecretKey;
use iroh_ssh::invite::{Code, InvitationRoute, SharedInvitation};

#[test]
fn round_trip_escapes_components_and_keeps_the_code_in_the_fragment() -> Result<()> {
    let endpoint = SecretKey::generate().public();
    let code = Code::generate();
    let expected = code.to_string();
    let route = InvitationRoute::new(endpoint, "sam+R&D", Some("https://relay.example/a+b"))?;
    let link = SharedInvitation { route, code }.to_string();
    assert!(link.contains("&user=sam%2BR%26D&relay=https%3A%2F%2Frelay.example%2Fa%2Bb%2F"));
    let (public, secret) = link.split_once('#').unwrap();
    assert!(!public.contains(&expected));
    assert_eq!(secret, expected);
    let result: SharedInvitation = link.parse()?;
    assert_eq!(result.route.endpoint, endpoint);
    assert_eq!(result.route.user, "sam+R&D");
    assert_eq!(
        result.route.relay.unwrap().as_str(),
        "https://relay.example/a+b/"
    );
    assert_eq!(result.code.to_string(), expected);
    let unicode = InvitationRoute::new(endpoint, "юзер", None)?;
    let plain = SharedInvitation {
        route: unicode,
        code: result.code,
    }
    .to_string();
    assert!(plain.parse::<SharedInvitation>()?.route.relay.is_none());
    Ok(())
}

#[test]
fn malformed_links_and_embedded_credentials_are_rejected_without_echoing_values() {
    let id = SecretKey::generate().public();
    let code = Code::generate().to_string();
    let head = format!("sshi://invite/v1?endpoint={id}&user=pi");
    for invalid in [
        format!("{head}&user=root#{code}"),
        format!("{head}&code={code}#{code}"),
        format!("{head}&relay=https%3A%2F%2Fa.example%2F%3Ftoken%3Dprivate-test-value#{code}"),
        format!("{head}&relay=https%3A%2F%2Froot%3Aprivate-test-value%40a.example#{code}"),
        format!("{head}&relay=https%3A%2F%2Fa.example%2F%23private-test-value#{code}"),
        format!("{head}&relay=file%3A%2F%2Fsomewhere#{code}"),
        format!("{head}&relay=https%3A%2F%2Fa.example%0A#{code}"),
        format!("sshi://invite/v1?user=pi#{code}"),
        format!("sshi://invite/v2?endpoint={id}&user=pi#{code}"),
        format!("sshi://invite/v1?endpoint=invalid&user=pi#{code}"),
        format!("sshi://invite/v1?endpoint={id}&user=%FF#{code}"),
        format!("sshi://invite/v1?endpoint={id}&user=%ZZ#{code}"),
        format!("sshi://invite/v1?endpoint={id}&user=%0A#{code}"),
        head.clone(),
        format!("{head}#junk"),
    ] {
        let err = invalid
            .parse::<SharedInvitation>()
            .err()
            .expect("invalid link accepted");
        assert!(!format!("{err:#}").contains(&code));
        assert!(!format!("{err:#}").contains("private-test-value"));
    }
}

#[cfg(unix)]
mod command {
    use super::*;
    use anyhow::{Context, ensure};
    use std::{
        fs,
        io::Write,
        os::unix::fs::PermissionsExt,
        path::Path,
        process::{Command, Output, Stdio},
    };

    fn cli(home: &Path) -> Command {
        let mut program = Command::new(env!("CARGO_BIN_EXE_sshi"));
        program
            .env_clear()
            .env("HOME", home)
            .env("SSHI_CONFIG", home.join("sshi.toml"));
        program
    }

    fn run(mut cmd: Command, args: &[&str], input: Option<&str>) -> Result<Output> {
        let mut child = cmd
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        if let Some(data) = input {
            child.stdin.take().unwrap().write_all(data.as_bytes())?;
        } else {
            drop(child.stdin.take());
        }
        Ok(child.wait_with_output()?)
    }

    fn success(out: Output) -> Result<String> {
        ensure!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(String::from_utf8(out.stdout)?.trim_end().to_owned())
    }

    #[test]
    fn share_and_inspect_are_offline_and_raw_invites_remain_supported() -> Result<()> {
        let receiver = tempfile::tempdir()?;
        let guest = tempfile::tempdir()?;
        fs::write(
            receiver.path().join("sshi.toml"),
            "relay_url = 'https://relay.example/'\n",
        )?;
        let guest_id = SecretKey::generate().public().to_string();
        let raw = success(run(cli(receiver.path()), &["invite", &guest_id], None)?)?;
        assert!(raw.parse::<Code>().is_ok());
        let args = ["invite", &guest_id, "--share", "--user", "pi"];
        let link = success(run(cli(receiver.path()), &args, None)?)?;
        let endpoint = success(run(cli(receiver.path()), &["id"], None)?)?;
        let inspect = |text: &str| run(cli(guest.path()), &["invitation", "inspect"], Some(text));
        let metadata = success(inspect(&link)?)?;
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&metadata)?,
            serde_json::json!({
                "user":"pi", "endpoint":endpoint, "relay":"https://relay.example/"
            })
        );
        let code = link.split_once('#').context("code fragment")?.1;
        assert_ne!(raw, code);
        assert!(!metadata.contains(code));
        assert!(!guest.path().join(".local").exists());

        let failure = |args: &[&str], input: &str, reason: &str| -> Result<()> {
            let out = run(cli(guest.path()), args, Some(input))?;
            assert!(!out.status.success());
            let err = String::from_utf8(out.stderr)?;
            assert!(err.contains(reason), "{err}");
            assert!(!err.contains(code));
            Ok(())
        };
        failure(
            &["invitation", "redeem"],
            code,
            "requires the receiver's Endpoint ID",
        )?;
        failure(
            &["invitation", "redeem", "--endpoint", &guest_id],
            &link,
            "differs from the invitation",
        )?;
        failure(
            &["invitation", "redeem", "--relay", "https://other.example/"],
            &link,
            "differs from the invitation",
        )?;
        failure(&["invitation", "inspect"], &"x".repeat(10000), "too long")?;
        assert!(!inspect(&format!("{link}\nextra line"))?.status.success());
        assert!(!guest.path().join(".local").exists());
        Ok(())
    }

    #[test]
    fn issuing_on_an_older_saved_receiver_wraps_its_raw_code_and_profile_relay() -> Result<()> {
        let home = tempfile::tempdir()?;
        let dir = home.path();
        fs::write(
            dir.join("sshi.toml"),
            "relay_url = 'https://default.example/'\n",
        )?;
        let peer = SecretKey::generate().public().to_string();
        let guest = SecretKey::generate().public().to_string();
        let raw = Code::generate().to_string();
        success(run(
            cli(dir),
            &[
                "profile",
                "save",
                "--name",
                "Workshop",
                "--user",
                "pi",
                "--endpoint",
                &peer,
                "--relay",
                "https://other.example/",
            ],
            None,
        )?)?;
        let ssh = dir.join("ssh");
        fs::write(&ssh, "#!/bin/sh\nprintf '%s\\n' \"$TEST_INVITE\"\n")?;
        fs::set_permissions(&ssh, fs::Permissions::from_mode(0o700))?;
        let mut issuer = cli(dir);
        issuer.env("PATH", dir).env("TEST_INVITE", &raw);
        let out = success(run(
            issuer,
            &[
                "issue-invite",
                "--share",
                "--name",
                "Workshop",
                "--client",
                &guest,
            ],
            None,
        )?)?;
        let link: SharedInvitation = out.parse()?;
        assert_eq!(link.route.endpoint.to_string(), peer);
        assert_eq!(link.route.user, "pi");
        assert_eq!(link.route.relay.unwrap().as_str(), "https://other.example/");
        assert_eq!(link.code.to_string(), raw);
        Ok(())
    }
}
