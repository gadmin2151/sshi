use crate::lang::{tr, tr_format};
use std::{
    env,
    io::{self, Write},
    path::PathBuf,
    process::Command,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use iroh::EndpointId;
use iroh_ssh::{
    RELAY_TOKEN_ENV, RELAY_URL_ENV, allow_client_command, env_value, install_ssh_key_command,
    invite::Code, issue_invitation_command, relay_url, tunnel,
};
use russh::{
    ChannelMsg, Disconnect, MethodKind,
    client::{self, AuthResult, KeyboardInteractiveAuthResponse},
    keys::{self, HashAlg, PrivateKey, PrivateKeyWithHashAlg, PublicKeyOrCertificate},
};
use tokio::{io::AsyncReadExt, time};
use windows_sys::Win32::{
    Foundation::HANDLE,
    Storage::FileSystem::{FILE_TYPE_DISK, FILE_TYPE_PIPE, GetFileType},
    System::Console::{
        ATTACH_PARENT_PROCESS, AllocConsole, AttachConsole, CONSOLE_SCREEN_BUFFER_INFO,
        ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT, ENABLE_PROCESSED_OUTPUT,
        ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode,
        GetConsoleScreenBufferInfo, GetConsoleWindow, GetStdHandle, STD_INPUT_HANDLE,
        STD_OUTPUT_HANDLE, SetConsoleCP, SetConsoleMode, SetConsoleOutputCP, SetConsoleTitleW,
    },
};

use crate::{
    broker::{self, Route},
    store::{DEFAULT_RELAY, Profile, Store, crypt, last_error, wide},
    window,
};

pub enum RelayAction {
    Shell,
    CopySshKey,
    CopyEndpointId(EndpointId),
    IssueInvite {
        client: EndpointId,
        service_user: Option<String>,
    },
}

pub fn open_window(title: &str) -> Result<()> {
    if unsafe { AllocConsole() } == 0 {
        return Err(last_error()).context(tr(
            "Открыть окно терминала Windows",
            "Open a Windows terminal",
        ));
    }
    let title = wide(format!("sshi — {title}"));
    unsafe {
        SetConsoleTitleW(title.as_ptr());
        SetConsoleCP(65001);
        SetConsoleOutputCP(65001);
    }
    Ok(())
}

pub fn has_console_input() -> bool {
    let mut mode = 0;
    (unsafe { GetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), &mut mode) }) != 0
}

pub fn open_command_window() -> Result<bool> {
    let output = unsafe { GetFileType(GetStdHandle(STD_OUTPUT_HANDLE)) };
    if has_console_input() || matches!(output, FILE_TYPE_PIPE | FILE_TYPE_DISK) {
        return Ok(false);
    }
    if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } != 0 {
        return Ok(false);
    }
    open_window(tr("Команды", "Commands"))?;
    Ok(true)
}

pub async fn connect(
    store: Store,
    profile: Profile,
    interactive: bool,
    action: RelayAction,
) -> Result<()> {
    let route = relay_route(&profile)?;
    eprintln!(
        "{}",
        tr_format!(
            "Подключение к {}@{} через {}",
            "Connecting to {}@{} through {}",
            profile.user,
            profile.endpoint,
            route.relay
        )
    );
    let outcome = async {
        let stream = broker::connect(&store, &route, profile.endpoint).await?;
        let config = client::Config {
            keepalive_interval: Some(Duration::from_secs(30)),
            ..Default::default()
        };
        let handler = VerifyHost {
            store: store.clone(),
            endpoint: profile.endpoint,
            interactive,
        };
        let mut ssh = client::connect_stream(Arc::new(config), stream, handler)
            .await
            .context(tr(
                "Установить SSH-соединение",
                "Establish the SSH connection",
            ))?;
        let result = match action {
            RelayAction::Shell => {
                authenticate(&mut ssh, &store, &profile, interactive, None).await?;
                if let Err(error) = store.record_connection(&profile) {
                    eprintln!(
                        "{}",
                        tr_format!(
                            "Сохранить историю подключений: {error:#}",
                            "Save connection history: {error:#}"
                        )
                    );
                }
                shell(&ssh, interactive).await.map(|()| None)
            }
            RelayAction::CopySshKey => copy_ssh_key(&mut ssh, &store, &profile, interactive)
                .await
                .map(|()| None),
            RelayAction::CopyEndpointId(id) => {
                authenticate(&mut ssh, &store, &profile, interactive, None).await?;
                copy_endpoint_id(&ssh, id).await.map(|()| None)
            }
            RelayAction::IssueInvite {
                client,
                service_user,
            } => {
                authenticate(&mut ssh, &store, &profile, interactive, None).await?;
                request_invitation(&ssh, profile.endpoint, client, service_user.as_deref())
                    .await
                    .map(Some)
            }
        };
        let _ = ssh.disconnect(Disconnect::ByApplication, "", "").await;
        if let Some(code) = result? {
            if interactive {
                if let Err(error) =
                    window::show_invitation(unsafe { GetConsoleWindow() }, &code.to_string())
                {
                    println!(
                        "{}",
                        tr_format!("Код приглашения\n{code}", "Invitation code\n{code}")
                    );
                    return Err(error);
                }
            } else {
                println!("{code}");
            }
        }
        Ok(())
    }
    .await;
    outcome.map_err(|error: anyhow::Error| {
        if let Some(denied) = error.downcast_ref::<tunnel::ClientNotAllowed>() {
            anyhow::anyhow!(tr_format!(
                "Получатель отклонил Endpoint ID этого клиента Windows\n{}\n\n\
                 Передайте этот ID администратору. С кодом приглашения нажмите «Ввести приглашение». \
                 При ручной настройке добавьте ID в allow_clients в /etc/sshi/config.toml. \
                 На старых версиях перезапустите службу пользователя, который обслуживает этот Endpoint ID. \
                 В Docker Compose используется IROH_SSH_CLIENT_ID.",
                "The receiver rejected this Windows client’s endpoint ID\n{}\n\nGive this ID to the administrator. If you have an invitation, choose “Redeem invitation”. For manual setup, add the ID to allow_clients in /etc/sshi/config.toml. Older versions require restarting the receiving user’s service. Docker Compose uses IROH_SSH_CLIENT_ID.",
                denied.client
            ))
        } else {
            error
        }
    })
}

pub async fn enroll(store: Store, profile: Profile, interactive: bool) -> Result<()> {
    let id = store.identity()?.public();
    eprintln!(
        "{}",
        tr_format!(
            "Разрешение Endpoint ID {id} на «{}»",
            "Authorizing endpoint ID {id} on “{}”",
            profile.name
        )
    );
    let route = relay_route(&profile)?;
    let code: Code = prompt(
        tr("Код приглашения: ", "Invitation code: ").into(),
        false,
        interactive,
    )
    .await?
    .parse()
    .context(tr("Проверьте код приглашения", "Check the invitation code"))?;
    broker::redeem(&store, &route, profile.endpoint, code)
        .await
        .context(tr(
            "Принять приглашение через Iroh relay",
            "Redeem the invitation through the Iroh relay",
        ))?;
    println!(
        "{}",
        tr(
            "Этот компьютер разрешён. Теперь можно подключиться через iroh.",
            "This computer is authorized. You can now connect through Iroh."
        )
    );
    Ok(())
}

pub async fn issue_invite(store: Store, profile: Profile, interactive: bool) -> Result<()> {
    let client = prompt(
        tr("Endpoint ID нового ПК: ", "New computer endpoint ID: ").into(),
        true,
        interactive,
    )
    .await?
    .trim()
    .parse()
    .context(tr(
        "Проверьте Endpoint ID нового ПК",
        "Check the new computer’s endpoint ID",
    ))?;
    let account = prompt(
        tr_format!(
            "Пользователь службы sshi [{}]: ",
            "SSHI service user [{}]: ",
            profile.user
        ),
        true,
        interactive,
    )
    .await?;
    let account = account.trim();
    ensure!(
        !account.starts_with('-')
            && !account
                .chars()
                .any(|ch| ch.is_control() || ch.is_whitespace()),
        tr(
            "Укажите одного пользователя службы sshi",
            "Enter one SSHI service user"
        )
    );
    let service_user = (!account.is_empty() && account != profile.user).then(|| account.to_owned());
    connect(
        store,
        profile,
        interactive,
        RelayAction::IssueInvite {
            client,
            service_user,
        },
    )
    .await
}

async fn request_invitation(
    ssh: &client::Handle<VerifyHost>,
    receiver: EndpointId,
    client: EndpointId,
    service_user: Option<&str>,
) -> Result<Code> {
    let command = issue_invitation_command(receiver, client, service_user)?;
    let mut stdout = Vec::new();
    exec_remote(ssh, command, None, |data, stderr| {
        if stderr {
            eprint!("{}", clean_prompt(&String::from_utf8_lossy(data)));
        } else {
            ensure!(
                stdout.len() + data.len() <= 4096,
                tr(
                    "Слишком длинный ответ получателя",
                    "Receiver response is too long"
                )
            );
            stdout.extend_from_slice(data);
        }
        Ok(())
    })
    .await
    .context(tr("Создать приглашение", "Create an invitation"))?;

    std::str::from_utf8(&stdout)?.trim().parse().context(tr(
        "Проверить код приглашения",
        "Validate the invitation code",
    ))
}

pub fn default_relay() -> Result<String> {
    Ok(env_value(RELAY_URL_ENV)?.unwrap_or_else(|| DEFAULT_RELAY.to_owned()))
}

fn relay_route(profile: &Profile) -> Result<Route> {
    let default = relay_url(&default_relay()?)
        .context(tr("Проверьте IROH_RELAY_URL", "Check IROH_RELAY_URL"))?;
    let relay = if profile.relay.is_empty() {
        default.clone()
    } else {
        relay_url(&profile.relay).context(tr("Адрес iroh relay", "Iroh relay address"))?
    };
    let token = if !profile.relay_token.is_empty() {
        Some(String::from_utf8(crypt(&profile.relay_token, false)?)?)
    } else if relay == default {
        env_value(RELAY_TOKEN_ENV)?
    } else {
        None
    };
    Ok(Route {
        relay: relay.to_string(),
        token,
    })
}

pub async fn proxy(store: &Store, profile: &Profile) -> Result<()> {
    let route = relay_route(profile)?;
    let (read, write) = broker::connect(store, &route, profile.endpoint)
        .await?
        .into_split();
    tunnel::proxy_stdio(read, write).await
}

pub fn forward(profile: &Profile, args: &[std::ffi::OsString]) -> Result<()> {
    ensure!(
        matches!(args, [flag, _rule] if matches!(flag.to_str(), Some("-L" | "-R" | "-D"))),
        "Invalid forwarding parameters"
    );
    let system = env::var_os("SystemRoot").context(tr(
        "Не удалось определить папку Windows",
        "Could not find the Windows directory",
    ))?;
    let ssh = PathBuf::from(system).join("System32/OpenSSH/ssh.exe");
    ensure!(
        ssh.is_file(),
        tr(
            "Для проброса портов установите дополнительный компонент Windows «Клиент OpenSSH».",
            "Install the Windows OpenSSH Client optional feature to forward ports."
        )
    );
    eprintln!(
        "{}",
        tr_format!(
            "Проброс портов через {}@{}",
            "Port forwarding through {}@{}",
            profile.user,
            profile.endpoint
        )
    );
    let executable = env::current_exe()?;
    let path = executable
        .to_str()
        .context(tr("Некорректный путь к программе", "Invalid program path"))?;
    // OpenSSH expands percent tokens before Win32 starts the proxy process.
    // Only the program path and a fixed switch enter this command string.
    let path = path
        .strip_prefix(r"\\?\")
        .unwrap_or(path)
        .replace('\\', "/")
        .replace('%', "%%");
    let mut command = Command::new(ssh);
    command.args(["-N", "-T", "-o", "ExitOnForwardFailure=yes", "-o"]);
    command.arg(format!("ProxyCommand=\"{path}\" --forward-proxy"));
    command.args(["-o", &format!("HostKeyAlias={}", profile.endpoint)]);
    if !profile.ssh_key.is_empty() {
        command.arg("-i").arg(&profile.ssh_key);
    }
    eprintln!(
        "{}",
        tr(
            "Запускается Windows OpenSSH. Для остановки нажмите Ctrl+C.",
            "Starting Windows OpenSSH. Press Ctrl+C to stop the tunnel."
        )
    );
    let status = command
        .args(args)
        .arg("--")
        .arg(format!("{}@{}", profile.user, profile.endpoint))
        .status()
        .context(tr("Запустить Windows OpenSSH", "Start Windows OpenSSH"))?;
    ensure!(
        status.success(),
        "{}",
        tr_format!(
            "OpenSSH завершился с кодом {status}",
            "OpenSSH exited with status {status}"
        )
    );
    Ok(())
}

async fn copy_endpoint_id(ssh: &client::Handle<VerifyHost>, id: EndpointId) -> Result<()> {
    run_remote(ssh, allow_client_command(id), None)
        .await
        .context(tr(
            "Записать Endpoint ID на получателе. Проверьте версию sshi и права учётной записи",
            "Save the endpoint ID on the receiver. Check the SSHI version and account permissions",
        ))
}

async fn copy_ssh_key(
    ssh: &mut client::Handle<VerifyHost>,
    store: &Store,
    profile: &Profile,
    interactive: bool,
) -> Result<()> {
    let private = if profile.ssh_key.is_empty() {
        store.ensure_ssh_identity()?
    } else {
        load_key(&profile.ssh_key, interactive).await?
    };
    let mut public = private.public_key().clone();
    public.set_comment("");
    let line = format!("{}\n", public.to_openssh()?);
    authenticate(ssh, store, profile, interactive, Some(private)).await?;
    run_remote(ssh, install_ssh_key_command(), Some(line.into_bytes()))
        .await
        .context(tr(
            "Добавить SSH-ключ в ~/.ssh/authorized_keys на получателе",
            "Add the SSH key to ~/.ssh/authorized_keys on the receiver",
        ))?;
    println!(
        "{}",
        tr_format!(
            "Публичный SSH-ключ установлен для пользователя {}.",
            "The public SSH key was installed for user {}.",
            profile.user
        )
    );
    Ok(())
}

async fn run_remote(
    ssh: &client::Handle<VerifyHost>,
    command: String,
    input: Option<Vec<u8>>,
) -> Result<()> {
    let mut output = TerminalOutput::default();
    exec_remote(ssh, command, input, |data, _| Ok(output.write(data)?)).await
}

async fn exec_remote(
    ssh: &client::Handle<VerifyHost>,
    command: String,
    input: Option<Vec<u8>>,
    mut output: impl FnMut(&[u8], bool) -> Result<()>,
) -> Result<()> {
    let mut channel = ssh.channel_open_session().await?;
    channel.exec(true, command).await?;
    if let Some(bytes) = input {
        channel.data_bytes(bytes).await?;
        channel.eof().await?;
    }
    let mut exit = None;
    while let Some(message) = channel.wait().await {
        match message {
            ChannelMsg::Data { data } => output(&data, false)?,
            ChannelMsg::ExtendedData { data, .. } => output(&data, true)?,
            ChannelMsg::ExitStatus { exit_status } => exit = Some(exit_status),
            ChannelMsg::Failure => bail!(tr(
                "Сервер отклонил удалённую команду",
                "The server rejected the remote command"
            )),
            ChannelMsg::Close => break,
            _ => {}
        }
    }
    let status = exit.context(tr(
        "Сервер закрыл канал без кода завершения",
        "The server closed the channel without an exit status",
    ))?;
    ensure!(
        status == 0,
        "{}",
        tr_format!(
            "Удалённая команда завершилась с кодом {status}",
            "The remote command exited with status {status}"
        )
    );
    Ok(())
}

struct VerifyHost {
    store: Store,
    endpoint: EndpointId,
    interactive: bool,
}

impl client::Handler for VerifyHost {
    type Error = anyhow::Error;

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool> {
        let fingerprint = key.public_key().fingerprint(HashAlg::Sha256).to_string();
        if let Some(saved) = self.store.host_fingerprint(self.endpoint)? {
            ensure!(
                saved == fingerprint,
                "{}",
                tr_format!(
                    "Ключ SSH-сервера изменился. Ожидался {saved}, получен {fingerprint}",
                    "The SSH host key changed. Expected {saved}, received {fingerprint}"
                )
            );
            return Ok(true);
        }
        let accepted = prompt(
            tr_format!("Первое подключение. Сверьте отпечаток SSH-сервера: {fingerprint}\nДоверять этому ключу? Введите yes или no: ", "First connection. Verify the SSH host key fingerprint: {fingerprint}\nTrust this key? Enter yes or no: "),
            true,
            self.interactive,
        ).await?;
        if accepted != "yes" {
            return Ok(false);
        }
        self.store
            .save_host_fingerprint(self.endpoint, &fingerprint)?;
        Ok(true)
    }
}

async fn authenticate(
    ssh: &mut client::Handle<VerifyHost>,
    store: &Store,
    profile: &Profile,
    interactive: bool,
    identity: Option<PrivateKey>,
) -> Result<()> {
    let user = &profile.user;
    let mut result = ssh.authenticate_none(user).await?;
    if result.success() {
        return Ok(());
    }
    if supports_method(&result, MethodKind::PublicKey) {
        let private = match identity {
            Some(key) => Some(key),
            None if !profile.ssh_key.is_empty() => {
                Some(load_key(&profile.ssh_key, interactive).await?)
            }
            None => store.ssh_identity()?,
        };
        if let Some(private) = private {
            let signer = PrivateKeyWithHashAlg::new(
                Arc::new(private),
                ssh.best_supported_rsa_hash().await?.flatten(),
            );
            result = ssh.authenticate_publickey(user, signer).await?;
            if result.success() {
                return Ok(());
            }
        }
    }
    if supports_method(&result, MethodKind::Password) {
        let password = prompt(
            tr_format!("Пароль {user}: ", "Password for {user}: "),
            false,
            interactive,
        )
        .await?;
        result = ssh.authenticate_password(user, password).await?;
        if result.success() {
            return Ok(());
        }
    }
    if supports_method(&result, MethodKind::KeyboardInteractive) {
        let mut response = ssh
            .authenticate_keyboard_interactive_start(user, None::<String>)
            .await?;
        loop {
            match response {
                KeyboardInteractiveAuthResponse::Success => return Ok(()),
                KeyboardInteractiveAuthResponse::Failure { .. } => break,
                KeyboardInteractiveAuthResponse::InfoRequest {
                    instructions,
                    prompts,
                    ..
                } => {
                    if !instructions.is_empty() {
                        eprintln!("{}", clean_prompt(&instructions));
                    }
                    let mut answers = Vec::with_capacity(prompts.len());
                    for question in prompts {
                        answers.push(
                            prompt(clean_prompt(&question.prompt), question.echo, interactive)
                                .await?,
                        );
                    }
                    response = ssh
                        .authenticate_keyboard_interactive_respond(answers)
                        .await?;
                }
            }
        }
    }
    bail!(
        "{}",
        tr_format!(
            "Сервер отклонил вход для {user}. Проверьте логин и SSH-ключ или пароль",
            "The server rejected the login for {user}. Check your username and SSH key or password"
        )
    )
}

async fn load_key(path: &str, interactive: bool) -> Result<PrivateKey> {
    match keys::load_secret_key(path, None) {
        Err(keys::Error::KeyIsEncrypted) => {
            let passphrase = prompt(
                tr("Фраза-пароль SSH-ключа: ", "SSH key passphrase: ").into(),
                false,
                interactive,
            )
            .await?;
            keys::load_secret_key(path, Some(&passphrase))
        }
        result => result,
    }
    .context(tr("Открыть SSH-ключ", "Open the SSH key"))
}

fn supports_method(result: &AuthResult, kind: MethodKind) -> bool {
    matches!(result, AuthResult::Failure { remaining_methods, .. } if remaining_methods.contains(&kind))
}

fn clean_prompt(text: &str) -> String {
    text.chars()
        .filter(|&ch| !ch.is_control() || matches!(ch, '\n' | '\t'))
        .collect()
}

async fn prompt(message: String, echo: bool, interactive: bool) -> Result<String> {
    tokio::task::spawn_blocking(move || {
        eprint!("{message}");
        io::stderr().flush()?;
        let hidden = if !echo && interactive {
            Some(ConsoleMode::change(STD_INPUT_HANDLE, ENABLE_ECHO_INPUT, 0)?)
        } else {
            None
        };
        let mut value = String::new();
        let count = io::stdin().read_line(&mut value)?;
        drop(hidden);
        if !echo {
            eprintln!();
        }
        ensure!(count > 0, tr("Ввод прерван", "Input was interrupted"));
        Ok(value.trim_end_matches(['\r', '\n']).to_owned())
    })
    .await
    .context(tr(
        "Прочитать ответ в терминале",
        "Read the terminal response",
    ))?
}

async fn shell(ssh: &client::Handle<VerifyHost>, interactive: bool) -> Result<()> {
    let mut dims = dimensions();
    let mut channel = ssh.channel_open_session().await?;
    channel
        .request_pty(true, "xterm-256color", dims.0, dims.1, 0, 0, &[])
        .await?;
    let reply = time::timeout(Duration::from_secs(15), channel.wait())
        .await
        .context(tr(
            "Сервер не ответил на запрос терминала",
            "The server did not answer the terminal request",
        ))?;
    ensure!(
        matches!(reply, Some(ChannelMsg::Success)),
        tr(
            "Сервер отклонил выделение SSH-терминала",
            "The server rejected the SSH terminal request"
        )
    );
    channel.request_shell(true).await?;

    let _console = if interactive {
        Some((
            ConsoleMode::change(
                STD_INPUT_HANDLE,
                ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT,
                ENABLE_VIRTUAL_TERMINAL_INPUT,
            )?,
            ConsoleMode::change(
                STD_OUTPUT_HANDLE,
                0,
                ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING,
            )?,
        ))
    } else {
        None
    };
    let mut input = tokio::io::stdin();
    let mut output = TerminalOutput::default();
    let mut buf = [0; 8192];
    let mut resize = time::interval(Duration::from_millis(250));
    let mut stdin_open = true;
    let mut exit = None;

    loop {
        tokio::select! {
            event = channel.wait() => match event {
                Some(ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. }) => output.write(&data)?,
                Some(ChannelMsg::ExitStatus { exit_status }) => exit = Some(exit_status),
                Some(ChannelMsg::Failure) => bail!(tr("Сервер отклонил запрос оболочки", "The server rejected the shell request")),
                Some(ChannelMsg::Close) | None => break,
                _ => {}
            },
            size = input.read(&mut buf), if stdin_open => match size? {
                0 => { channel.eof().await?; stdin_open = false; }
                size => channel.data_bytes(buf[..size].to_vec()).await?,
            },
            _ = resize.tick(), if interactive => {
                let current = dimensions();
                if current != dims {
                    channel.window_change(current.0, current.1, 0, 0).await?;
                    dims = current;
                }
            }
        }
    }
    ensure!(
        exit.unwrap_or(0) == 0,
        "{}",
        tr_format!(
            "Удалённая оболочка завершилась с кодом {}",
            "The remote shell exited with status {}",
            exit.unwrap_or(0)
        )
    );
    Ok(())
}

fn dimensions() -> (u32, u32) {
    let mut info: CONSOLE_SCREEN_BUFFER_INFO = unsafe { std::mem::zeroed() };
    if unsafe { GetConsoleScreenBufferInfo(GetStdHandle(STD_OUTPUT_HANDLE), &mut info) } == 0 {
        return (100, 30);
    }
    (
        (i32::from(info.srWindow.Right) - i32::from(info.srWindow.Left) + 1).max(1) as u32,
        (i32::from(info.srWindow.Bottom) - i32::from(info.srWindow.Top) + 1).max(1) as u32,
    )
}

struct ConsoleMode(HANDLE, u32);

impl ConsoleMode {
    fn change(which: u32, clear: u32, set: u32) -> Result<Self> {
        let handle = unsafe { GetStdHandle(which) };
        let mut original = 0;
        if unsafe { GetConsoleMode(handle, &mut original) } == 0
            || unsafe { SetConsoleMode(handle, (original & !clear) | set) } == 0
        {
            return Err(last_error()).context(tr(
                "Настроить терминал Windows",
                "Configure the Windows terminal",
            ));
        }
        Ok(Self(handle, original))
    }
}

impl Drop for ConsoleMode {
    fn drop(&mut self) {
        unsafe { SetConsoleMode(self.0, self.1) };
    }
}

#[derive(Default)]
struct TerminalOutput {
    pending: Vec<u8>,
}

impl TerminalOutput {
    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        self.pending.extend_from_slice(data);
        let mut out = io::stdout().lock();
        let mut offset = 0;
        // Windows console writes require complete UTF-8 characters at each call.
        while offset < self.pending.len() {
            let bytes = &self.pending[offset..];
            match std::str::from_utf8(bytes) {
                Ok(text) => {
                    out.write_all(text.as_bytes())?;
                    offset = self.pending.len();
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    out.write_all(&bytes[..valid])?;
                    offset += valid;
                    match error.error_len() {
                        Some(invalid) => {
                            out.write_all("�".as_bytes())?;
                            offset += invalid;
                        }
                        None => break,
                    }
                }
            }
        }
        self.pending.drain(..offset);
        out.flush()
    }
}
