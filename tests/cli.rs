#![cfg(unix)]

use std::{
    fs,
    io::Write,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{Context, Result, ensure};
use iroh::SecretKey;
use serde_json::Value;

struct Fixture(tempfile::TempDir);

impl Fixture {
    fn new() -> Result<Self> {
        let dir = tempfile::tempdir()?;
        fs::write(
            dir.path().join("config.toml"),
            "relay_url = 'https://base.example/'\n",
        )?;
        Ok(Self(dir))
    }

    fn dir(&self) -> &Path {
        self.0.path()
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_sshi"));
        cmd.env_remove("IROH_RELAY_URL")
            .env_remove("IROH_RELAY_ACCESS_TOKEN")
            .env_remove("SSHI_PROFILE_RELAY_URL")
            .env("HOME", self.dir())
            .env("SSHI_CONFIG", self.dir().join("config.toml"))
            .env("XDG_CONFIG_HOME", self.dir().join("config"))
            .env("XDG_STATE_HOME", self.dir().join("state"));
        cmd
    }

    fn run(&self, args: &[&str]) -> Result<String> {
        let out = self.command().args(args).output()?;
        ensure!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(String::from_utf8(out.stdout)?.trim_end().to_owned())
    }

    fn json(&self, args: &[&str]) -> Result<Value> {
        Ok(serde_json::from_str(&self.run(args)?)?)
    }
}

#[test]
fn relay_overrides_use_only_credentials_for_the_selected_route() -> Result<()> {
    let f = Fixture::new()?;
    let receiver = SecretKey::generate().public().to_string();
    let dir = f.dir().join("config/sshi");
    fs::create_dir_all(&dir)?;
    fs::write(dir.join("relay.token"), "test-fixture-value")?;
    let base = f.json(&["config", "show", "--json"])?;
    assert_eq!(
        base["relay_token_file"],
        dir.join("relay.token").to_str().unwrap()
    );
    f.run(&[
        "profile",
        "save",
        "--name",
        "-lab",
        "--user",
        "pi",
        "--endpoint",
        &receiver,
        "--relay",
        "https://secondary.example/",
    ])?;
    let args = [
        "profile", "run", "--name", "-lab", "--", "config", "show", "--json",
    ];
    let secondary = f.json(&args)?;
    assert_eq!(secondary["relay_url"], "https://secondary.example/");
    let scoped = Path::new(secondary["relay_token_file"].as_str().context("file")?);
    assert_ne!(scoped, dir.join("relay.token"));
    assert!(!scoped.exists());
    let standalone = f
        .command()
        .env("IROH_RELAY_URL", "https://base.example/")
        .env("IROH_RELAY_ACCESS_TOKEN", "ephemeral-test-value")
        .args(args)
        .output()?;
    assert!(standalone.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&standalone.stdout)?["relay_token_file"],
        secondary["relay_token_file"]
    );
    f.run(&["config", "set-relay", "https://secondary.example/"])?;
    let mut input = f
        .command()
        .args(["config", "set-token"])
        .stdin(Stdio::piped())
        .spawn()?;
    input
        .stdin
        .take()
        .unwrap()
        .write_all(b"other-fixture-value\n")?;
    assert!(input.wait()?.success());
    assert_eq!(fs::read_to_string(scoped)?, "other-fixture-value\n");
    assert_eq!(fs::metadata(scoped)?.mode() & 0o777, 0o600);
    Ok(())
}

#[test]
fn backup_restores_named_connections_and_legacy_history_without_identity_files() -> Result<()> {
    let f = Fixture::new()?;
    let receiver = f.run(&["id"])?;
    f.run(&[
        "profile",
        "save",
        "--name",
        "Studio",
        "--user",
        "pi",
        "--endpoint",
        &receiver,
    ])?;
    f.run(&[
        "profile",
        "save",
        "--previous",
        "Studio",
        "--name",
        "Studio Mac",
        "--user",
        "worker",
        "--endpoint",
        &receiver,
    ])?;
    let state = f.dir().join("state/sshi");
    let first = format!("pi@{receiver}");
    let second = format!("worker@{receiver}");
    fs::write(
        state.join("history"),
        format!("{first}\n{first}\n{second}\n"),
    )?;
    let path = f.dir().join("connections.json");
    let backup = path.to_str().unwrap();
    f.run(&["backup", backup])?;
    let exported: Value = serde_json::from_slice(&fs::read(path.clone())?)?;
    assert_eq!(exported["version"], 1);
    assert_eq!(exported["history"], serde_json::json!([second, first]));
    assert_eq!(exported["connections"].as_array().unwrap().len(), 1);
    let fresh = Fixture::new()?;
    for _ in 0..2 {
        fresh.run(&["restore", backup])?;
    }
    assert_eq!(fresh.run(&["list"])?, format!("{second}\n{first}"));
    assert_eq!(
        fresh.json(&["profile", "list", "--json"])?,
        exported["connections"]
    );
    assert!(!fresh.dir().join("state/sshi/endpoint.key").exists());
    assert_ne!(fresh.run(&["id"])?, receiver);
    // Check that names containing spaces are forwarded as one argument and
    // that a profile command really selects the persisted SSH user and endpoint.
    let ssh = f.dir().join("ssh");
    fs::write(&ssh, "#!/bin/sh\nprintf '%s\\n' \"$@\"\n")?;
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o700))?;
    let ssh_args = f
        .command()
        .env("PATH", f.dir())
        .args(["connect", "Studio Mac", "hostname"])
        .output()?;
    assert!(
        ssh_args.status.success(),
        "{}",
        String::from_utf8_lossy(&ssh_args.stderr)
    );
    assert!(
        String::from_utf8(ssh_args.stdout)?
            .ends_with(&format!("-l\nworker\n--\n{receiver}\nhostname\n"))
    );
    f.run(&["profile", "remove", "--name", "studio mac"])?;
    assert_eq!(
        f.json(&["profile", "list", "--json"])?,
        serde_json::json!([])
    );
    Ok(())
}

#[test]
fn rename_changes_only_the_profile_name_and_rejects_conflicts_atomically() -> Result<()> {
    let f = Fixture::new()?;
    let id = SecretKey::generate().public().to_string();
    for name in ["Office", "Studio"] {
        f.run(&[
            "profile",
            "save",
            "--name",
            name,
            "--user",
            "operator",
            "--endpoint",
            &id,
            "--relay",
            "https://office.example/",
        ])?;
    }
    let args = ["profile", "list", "--json"];
    let mut expected = f.json(&args)?;
    for invalid in ["sTUDIO", "", " trailing "] {
        let out = f
            .command()
            .args(["profile", "rename", "--name", "office", "--to", invalid])
            .output()?;
        assert!(!out.status.success());
        assert_eq!(f.json(&args)?, expected);
    }
    for (old, new) in [("Office", "OFFICE"), ("office", "-Рабочий ПК")] {
        f.run(&["profile", "rename", "--name", old, "--to", new])?;
        expected[0]["name"] = new.into();
        assert_eq!(f.json(&args)?, expected);
    }
    Ok(())
}

#[test]
fn gui_forward_uses_the_saved_route_and_native_ssh_arguments() -> Result<()> {
    let f = Fixture::new()?;
    let id = SecretKey::generate().public().to_string();
    f.run(&[
        "profile",
        "save",
        "--name",
        "GUI profile",
        "--user",
        "operator",
        "--endpoint",
        &id,
        "--relay",
        "https://work.example/",
    ])?;
    let ssh = f.dir().join("ssh");
    fs::write(
        &ssh,
        "#!/bin/sh\nprintf '%s\\n' \"$SSHI_PROFILE_RELAY_URL\" \"$@\"\n",
    )?;
    fs::set_permissions(ssh, fs::Permissions::from_mode(0o700))?;
    for (kind, flag, rule) in [
        ("local", "-L", "[::1]:8080:db.example:80"),
        ("remote", "-R", "[::1]:8080:db.example:80"),
        ("dynamic", "-D", "[::1]:8080"),
    ] {
        let mut cmd = f.command();
        cmd.env("PATH", f.dir()).args([
            "forward",
            "--profile",
            "GUI profile",
            "--kind",
            kind,
            "--bind-host",
            "::1",
            "--bind-port",
            "8080",
        ]);
        if kind != "dynamic" {
            cmd.args(["--target-host", "db.example", "--target-port", "80"]);
        }
        let output = cmd.output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout)?;
        let args: Vec<_> = stdout.lines().collect();
        assert_eq!(args[0], "https://work.example/");
        assert!(args.contains(&"ExitOnForwardFailure=yes"));
        assert!(args.contains(&"-N"));
        assert!(args.windows(2).any(|w| w == [flag, rule]));
        assert!(args.ends_with(&["-l", "operator", "--", &id]));
        assert!(
            args.iter()
                .any(|s| s.contains("ProxyCommand=") && s.contains("__proxy"))
        );
    }
    Ok(())
}

#[test]
fn simultaneous_profile_saves_and_key_install_are_idempotent() -> Result<()> {
    let f = Fixture::new()?;
    let id = SecretKey::generate().public().to_string();
    let mut saves = Vec::new();
    for i in 0..8 {
        saves.push(
            f.command()
                .args([
                    "profile",
                    "save",
                    "--name",
                    &format!("work {i}"),
                    "--user",
                    "pi",
                    "--endpoint",
                    &id,
                ])
                .spawn()?,
        );
    }
    for mut save in saves {
        assert!(save.wait()?.success());
    }
    assert_eq!(
        f.json(&["profile", "list", "--json"])?
            .as_array()
            .unwrap()
            .len(),
        8
    );
    let key = f.dir().join("test_key");
    assert!(
        Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(&key)
            .status()?
            .success()
    );
    let public = fs::read(key.with_extension("pub"))?;
    let mut installs = Vec::new();
    for _ in 0..3 {
        let mut child = f
            .command()
            .arg("__install-key")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()?;
        child.stdin.take().unwrap().write_all(&public)?;
        installs.push(child);
    }
    for mut job in installs {
        assert!(job.wait()?.success());
    }
    assert_eq!(fs::read(f.dir().join(".ssh/authorized_keys"))?, public);
    Ok(())
}
