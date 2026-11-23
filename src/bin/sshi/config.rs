use std::{
    env, fs,
    fs::{DirBuilder, File, OpenOptions},
    io::{self, ErrorKind, IsTerminal, Write},
    net::SocketAddr,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt, fchown},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use iroh::{EndpointId, RelayUrl};
use iroh_ssh::{
    DEFAULT_SSH_TARGET, RELAY_TOKEN_ENV, RELAY_URL_ENV, env_value, relay_url, ssh_target,
};
use serde::Deserialize;
use tempfile::NamedTempFile;
use toml_edit::{Array, DocumentMut, value};

use crate::state::xdg_dir;

const CONFIG_PATH: &str = "/etc/sshi/config.toml";

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    relay_url: Option<String>,
    relay_token_file: Option<PathBuf>,
    ssh_target: Option<String>,
    pub allow_clients: Vec<EndpointId>,
}

impl Config {
    fn path(override_path: Option<&std::ffi::OsStr>) -> PathBuf {
        override_path.map_or_else(|| PathBuf::from(CONFIG_PATH), PathBuf::from)
    }

    pub fn load() -> Result<Self> {
        let override_path = env::var_os("SSHI_CONFIG");
        let path = Self::path(override_path.as_deref());
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound && override_path.is_none() => {
                return Ok(Self::default());
            }
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        };
        Self::parse(&text, &path)
    }

    fn parse(text: &str, path: &Path) -> Result<Self> {
        toml::from_str(text).map_err(|error| {
            anyhow::anyhow!("invalid TOML in {} ({})", path.display(), error.message())
        })
    }

    pub fn allow_client(id: EndpointId) -> Result<()> {
        let path = Self::path(env::var_os("SSHI_CONFIG").as_deref());
        let path = fs::canonicalize(&path)
            .with_context(|| format!("find receiver configuration {}", path.display()))?;
        let directory = path.parent().context("find configuration directory")?;
        let mut lock_path = path.as_os_str().to_owned();
        lock_path.push(".lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(lock_path)
            .with_context(|| format!("open configuration lock for {}", path.display()))?;
        lock.lock().context("lock receiver configuration")?;

        let metadata = fs::metadata(&path)?;
        ensure!(metadata.is_file(), "{} must be a file", path.display());
        let original =
            fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        if Self::parse(&original, &path)?.allow_clients.contains(&id) {
            println!("Endpoint ID {id} is already allowed in {}", path.display());
            return Ok(());
        }
        let mut document: DocumentMut = original
            .parse()
            .with_context(|| format!("edit TOML in {}", path.display()))?;
        document
            .entry("allow_clients")
            .or_insert_with(|| value(Array::new()))
            .as_array_mut()
            .context("allow_clients must be a TOML array")?
            .push(id.to_string());
        let updated = document.to_string();
        Self::parse(&updated, &path)?;

        let mut temporary =
            NamedTempFile::new_in(directory).context("prepare new configuration")?;
        let owner = temporary.as_file().metadata()?;
        if metadata.uid() != owner.uid() || metadata.gid() != owner.gid() {
            fchown(
                temporary.as_file(),
                Some(metadata.uid()),
                Some(metadata.gid()),
            )
            .context("preserve the configuration owner")?;
        }
        temporary
            .as_file()
            .set_permissions(metadata.permissions())?;
        temporary.write_all(updated.as_bytes())?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(&path)
            .with_context(|| format!("replace {}", path.display()))?;
        File::open(directory)?.sync_all()?;
        println!("Allowed Endpoint ID {id} in {}", path.display());
        Ok(())
    }

    fn default_relay(&self) -> Result<Option<String>> {
        if let Some(url) = env_value(RELAY_URL_ENV)? {
            return Ok(Some(url));
        }
        if let Some(url) = read_optional(&user_dir()?.join("relay.url"))? {
            return Ok(Some(url.trim().to_owned()));
        }
        Ok(self.relay_url.clone())
    }

    pub fn relay(&self) -> Result<(RelayUrl, Option<String>)> {
        let base = self.default_relay()?;
        let selected = env_value(crate::profiles::PROFILE_RELAY)?
            .or_else(|| base.clone())
            .context("no relay configured; use sshi config set-relay URL")?;
        let url = relay_url(&selected).context("invalid relay_url")?;
        let layout = self.token_source(&url, base.as_deref())?;
        let token = match layout {
            TokenSource::Environment => env_value(RELAY_TOKEN_ENV)?,
            TokenSource::File(path, required) => match read_optional(&path)? {
                None if required => bail!("relay token file {} does not exist", path.display()),
                value => value.map(|v| v.trim_end_matches(['\r', '\n']).to_owned()),
            },
        };
        if let Some(token) = token.as_deref() {
            validate_token(token)?;
            ensure!(
                url.scheme() == "https",
                "an authenticated relay requires HTTPS"
            );
        }
        Ok((url, token))
    }

    pub fn ssh_target(&self) -> Result<SocketAddr> {
        ssh_target(self.ssh_target.as_deref().unwrap_or(DEFAULT_SSH_TARGET))
            .context("invalid ssh_target")
    }

    fn token_source(&self, url: &RelayUrl, base: Option<&str>) -> Result<TokenSource> {
        // A portable profile may specify another relay. Never forward credentials belonging
        // to the caller's configured route when opening an imported profile.
        let scoped = token_path(url)?;
        let env_url = env_value(RELAY_URL_ENV)?;
        let system_url = self.relay_url.as_deref().map(relay_url).transpose()?;
        let same_default = base.map(relay_url).transpose()?.as_ref() == Some(url);
        let established = env_url.as_deref().map(relay_url).transpose()?.as_ref() == Some(url)
            || system_url.as_ref() == Some(url);
        if same_default
            && (established || env_url.is_none() && system_url.is_none())
            && env_value(RELAY_TOKEN_ENV)?.is_some()
        {
            return Ok(TokenSource::Environment);
        }
        if scoped.try_exists()? {
            return Ok(TokenSource::File(scoped, true));
        }
        if same_default
            && (system_url.as_ref() == Some(url)
                || system_url.is_none() && !user_dir()?.join("relay.url").try_exists()?)
        {
            if let Some(path) = &self.relay_token_file {
                ensure!(
                    path.is_absolute(),
                    "relay_token_file must be an absolute path"
                );
                return Ok(TokenSource::File(path.clone(), true));
            }
            return Ok(TokenSource::File(user_dir()?.join("relay.token"), false));
        }
        Ok(TokenSource::File(scoped, false))
    }
}

enum TokenSource {
    Environment,
    File(PathBuf, bool),
}

fn read_optional(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(value) => Ok(Some(value)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

fn user_dir() -> Result<PathBuf> {
    Ok(xdg_dir("XDG_CONFIG_HOME", ".config")?.join("sshi"))
}

fn token_path(url: &RelayUrl) -> Result<PathBuf> {
    let hash = blake3::hash(url.as_str().as_bytes()).to_hex();
    Ok(user_dir()?.join(format!("relay-{}.token", &hash[..32])))
}

fn validate_token(token: &str) -> Result<()> {
    ensure!(
        !token.is_empty() && !token.chars().any(|c| c.is_control() || c.is_whitespace()),
        "relay token must contain one non-empty value with no whitespace"
    );
    Ok(())
}

#[derive(clap::Subcommand)]
pub enum Settings {
    /// Show effective settings. Secret token values are never printed.
    Show {
        #[arg(long)]
        json: bool,
    },
    /// Set the relay for this account.
    SetRelay { url: String },
    /// Remove the user relay override and use the system or environment setting.
    ResetRelay,
    /// Save a token for the current relay (or --relay) from a prompt or standard input.
    SetToken {
        #[arg(long)]
        relay: Option<String>,
    },
}

fn write_preference(path: &Path, value: &str) -> Result<()> {
    let dir = path.parent().context("no preferences directory")?;
    DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let mut output = NamedTempFile::new_in(dir)?;
    output
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    writeln!(output, "{value}")?;
    output.as_file().sync_all()?;
    output.persist(path)?;
    File::open(dir)?.sync_all()?;
    Ok(())
}

pub fn settings(command: Settings) -> Result<()> {
    let config = Config::load()?;
    match command {
        Settings::Show { json } => {
            let base = config.default_relay()?;
            let selected = env_value(crate::profiles::PROFILE_RELAY)?.or_else(|| base.clone());
            let overrides = user_dir()?.join("relay.url");
            let relay_source = if env_value(crate::profiles::PROFILE_RELAY)?.is_some() {
                "profile"
            } else if env_value(RELAY_URL_ENV)?.is_some() {
                "environment"
            } else if overrides.try_exists()? {
                "user"
            } else if config.relay_url.is_some() {
                "system"
            } else {
                "unset"
            };
            let (token, token_source) = match selected.as_deref().map(relay_url).transpose()? {
                Some(url) => match config.token_source(&url, base.as_deref())? {
                    TokenSource::Environment => (None, "environment"),
                    TokenSource::File(path, _) => {
                        let source = if path.try_exists()? { "file" } else { "unset" };
                        (Some(path), source)
                    }
                },
                None => (None, "unset"),
            };
            let data = serde_json::json!({
                "config_path": Config::path(env::var_os("SSHI_CONFIG").as_deref()),
                "relay_url": selected.unwrap_or_default(),
                "relay_source": relay_source,
                "relay_override_file": overrides,
                "relay_token_source": token_source,
                "relay_token_file": token,
                "ssh_target": config.ssh_target()?.to_string(),
                "allow_clients": config.allow_clients,
            });
            if json {
                println!("{}", serde_json::to_string(&data)?);
            } else {
                println!("{}", serde_json::to_string_pretty(&data)?);
            }
        }
        Settings::SetRelay { url } => {
            let relay = relay_url(&url).context("invalid relay URL")?;
            ensure!(
                relay.query().is_none() && relay.fragment().is_none(),
                "relay URL may not contain a query or fragment"
            );
            if let Some(active) = env_value(RELAY_URL_ENV)? {
                ensure!(
                    relay_url(&active)? == relay,
                    "unset IROH_RELAY_URL before changing the active default relay"
                );
            }
            write_preference(&user_dir()?.join("relay.url"), relay.as_str())?;
        }
        Settings::ResetRelay => {
            let path = user_dir()?.join("relay.url");
            match fs::remove_file(&path) {
                Ok(()) => File::open(user_dir()?)?.sync_all()?,
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(e) => return Err(e).with_context(|| format!("remove {}", path.display())),
            }
        }
        Settings::SetToken { relay } => {
            let base = config.default_relay()?;
            let selected = relay
                .or_else(|| base.clone())
                .context("set a relay URL first")?;
            let url = relay_url(&selected)?;
            ensure!(
                url.scheme() == "https",
                "an authenticated relay requires HTTPS"
            );
            ensure!(
                !matches!(
                    config.token_source(&url, base.as_deref())?,
                    TokenSource::Environment
                ),
                "unset IROH_RELAY_ACCESS_TOKEN before replacing the active token"
            );
            let mut text = String::new();
            if io::stdin().is_terminal() {
                text = rpassword::prompt_password("Relay access token: ")?;
            } else {
                io::stdin().read_line(&mut text)?;
            }
            let token = text.trim_end_matches(['\r', '\n']);
            validate_token(token)?;
            write_preference(&token_path(&url)?, token)?;
        }
    }
    Ok(())
}
