use crate::{
    ProxyMode, control_socket, profiles, quoted_sshi_executable, ssh_over_iroh, state::State,
};
use anyhow::{Context, Result};
use iroh_ssh::{
    file_transfer::{self, CopyArgs, Event, Request},
    profiles::{Profile, destination},
};
use russh_sftp::client::SftpSession;
use std::{
    io::{self, IsTerminal, Write},
    path::Path,
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    process::{Child, Command},
    time::timeout,
};

struct Connection {
    sftp: SftpSession,
    _ssh: Child,
}

async fn connect(profile: &Profile, identity: Option<&Path>, batch: bool) -> Result<Connection> {
    // An explicitly supplied private key must be used for this login.
    let socket = if identity.is_none() {
        control_socket::existing(profile)?
    } else {
        None
    };
    let mut ssh = ssh_over_iroh(
        profile.endpoint,
        &quoted_sshi_executable()?,
        ProxyMode::Standard,
        socket.as_deref(),
    );
    profiles::set_relay(&mut ssh, profile);
    ssh.args([
        "-T",
        "-s",
        "-o",
        "ControlMaster=no",
        "-o",
        "ClearAllForwardings=yes",
        "-o",
        "ForwardAgent=no",
        "-o",
        "ForwardX11=no",
        "-o",
        "PermitLocalCommand=no",
        "-o",
        "ConnectTimeout=25",
        "-o",
        "ServerAliveInterval=30",
        "-o",
        "ServerAliveCountMax=2",
    ]);
    if batch {
        ssh.args(["-o", "BatchMode=yes"]);
    }
    if let Some(identity) = identity {
        ssh.arg("-i").arg(identity);
    }
    ssh.arg("-l")
        .arg(&profile.user)
        .arg("--")
        .arg(profile.endpoint.to_string())
        .arg("sftp");

    let mut ssh = Command::from(ssh)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(if batch {
            Stdio::piped()
        } else {
            Stdio::inherit()
        })
        .kill_on_drop(true)
        .spawn()
        .context("start OpenSSH for file transfer")?;
    let diagnostics = ssh.stderr.take().map(|mut stderr| {
        tokio::spawn(async move {
            let mut tail = Vec::new();
            let mut chunk = [0; 4096];
            while let Ok(n @ 1..) = stderr.read(&mut chunk).await {
                tail.extend_from_slice(&chunk[..n]);
                if tail.len() > 4096 {
                    tail.drain(..tail.len() - 4096);
                }
            }
            String::from_utf8_lossy(&tail)
                .trim()
                .chars()
                .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
                .collect::<String>()
        })
    });
    let stream = tokio::io::join(
        ssh.stdout.take().context("read SSH output")?,
        ssh.stdin.take().context("write SSH input")?,
    );
    let sftp = match file_transfer::open_session(stream, if batch { 35 } else { 180 }).await {
        Ok(sftp) => sftp,
        Err(error) => {
            if timeout(Duration::from_secs(1), ssh.wait()).await.is_err() {
                let _ = ssh.kill().await;
            }
            let details = if let Some(task) = diagnostics {
                timeout(Duration::from_secs(1), task)
                    .await
                    .ok()
                    .and_then(|result| result.ok())
                    .unwrap_or_default()
            } else {
                String::new()
            };
            return if details.is_empty() {
                Err(error)
            } else {
                Err(error).context(details)
            };
        }
    };
    if let Ok(state) = State::load() {
        let _ = state.record(&profile.destination());
    }
    Ok(Connection { sftp, _ssh: ssh })
}

pub async fn copy(args: CopyArgs) -> Result<()> {
    let (target, transfer) = args.transfer()?;
    let profile = match destination(target) {
        Ok((user, endpoint)) => Profile {
            name: target.into(),
            user: user.into(),
            endpoint,
            relay: String::new(),
            notes: String::new(),
        },
        Err(_) => profiles::lookup(target)?,
    };
    let session = connect(
        &profile,
        args.identity.as_deref(),
        !io::stderr().is_terminal(),
    )
    .await?;
    let count = file_transfer::copy(
        &session.sftp,
        transfer,
        args.recursive,
        file_transfer::print_progress,
    )
    .await?;
    session.sftp.close().await.context("close file transfer")?;
    eprintln!("Copied {count} file(s)");
    Ok(())
}

fn send(event: Event) -> Result<()> {
    let mut out = io::stdout().lock();
    serde_json::to_writer(&mut out, &event)?;
    out.write_all(b"\n")?;
    Ok(out.flush()?)
}

pub async fn browser(profile: &str) -> Result<()> {
    let profile = profiles::lookup(profile)?;
    let mut input = BufReader::new(tokio::io::stdin()).lines();
    let mut connection: Option<Connection> = None;
    while let Some(line) = input.next_line().await? {
        let result = async {
            let request: Request =
                serde_json::from_str(&line).context("read file window request")?;
            if request.reconnects() {
                connection = None;
            }
            if request.needs_sftp() && connection.is_none() {
                connection = Some(connect(&profile, None, true).await?);
            }
            file_transfer::handle(request, connection.as_ref().map(|c| &c.sftp), |event| {
                let _ = send(event);
            })
            .await
        }
        .await;
        send(result.unwrap_or_else(|error| Event::Error {
            message: format!("{error:#}"),
        }))?;
    }
    if let Some(connection) = connection {
        let _ = connection.sftp.close().await;
    }
    Ok(())
}
