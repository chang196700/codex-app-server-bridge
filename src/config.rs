use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    net::IpAddr,
    path::{Path, PathBuf},
    time::Duration,
};

pub const EXAMPLE: &str = r#"# ~/.codex-app-server-bridge (TOML file, not a directory)
# Defaults to $CODEX_HOME, otherwise ~/.codex.
# codex_home = "~/.codex"
# socket_path = "~/.codex/app-server-control/app-server-control.sock"

# Only loopback addresses are accepted. Use an SSH tunnel for remote access.
host = "127.0.0.1"
# 0 = choose an available port; use e.g. 8765 for a fixed port.
port = 0
poll_interval_ms = 2000
connect_timeout_ms = 3000
max_connections = 64
max_message_size = 67108864
# Opt in to automatically setting CODEX_APP_SERVER_WS_URL for the user.
publish_codex_app_server_ws_url_environment = false
# Runtime state and shell environment exports live here.
# state_dir = "~/.local/state/codex-app-server-bridge"
"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub codex_home: Option<PathBuf>,
    pub socket_path: Option<PathBuf>,
    pub user_home: Option<PathBuf>,
    pub state_dir: Option<PathBuf>,
    pub windows_user_sid: Option<String>,
    pub host: IpAddr,
    pub port: u16,
    pub poll_interval_ms: u64,
    pub connect_timeout_ms: u64,
    pub max_connections: usize,
    pub max_message_size: usize,
    pub publish_codex_app_server_ws_url_environment: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            codex_home: None,
            socket_path: None,
            user_home: None,
            state_dir: None,
            windows_user_sid: None,
            host: IpAddr::from([127, 0, 0, 1]),
            port: 0,
            poll_interval_ms: 2000,
            connect_timeout_ms: 3000,
            max_connections: 64,
            max_message_size: 64 * 1024 * 1024,
            publish_codex_app_server_ws_url_environment: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Settings {
    pub config: Config,
    pub user_home: PathBuf,
    pub codex_home: PathBuf,
    pub socket_path: PathBuf,
    pub state_dir: PathBuf,
}

impl Settings {
    pub fn load(
        config_path: Option<&Path>,
        user_home: Option<&Path>,
        codex_home: Option<&Path>,
        port: Option<u16>,
    ) -> Result<Self> {
        let default_home = user_home
            .map(Path::to_path_buf)
            .or_else(dirs::home_dir)
            .context("cannot determine user home; pass --user-home")?;
        let path = absolute(
            config_path.unwrap_or(&default_home.join(".codex-app-server-bridge")),
            &default_home,
        )?;
        let mut config: Config = match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text)
                .with_context(|| format!("invalid TOML in {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && config_path.is_none() => {
                Config::default()
            }
            Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
        };
        let user_home = absolute(
            user_home
                .or(config.user_home.as_deref())
                .unwrap_or(&default_home),
            &default_home,
        )?;
        let env_codex = std::env::var_os("CODEX_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        let codex_home = absolute(
            codex_home
                .or(config.codex_home.as_deref())
                .or(env_codex.as_deref())
                .unwrap_or(&user_home.join(".codex")),
            &user_home,
        )?;
        let socket_path = absolute(
            config
                .socket_path
                .as_deref()
                .unwrap_or(&codex_home.join("app-server-control/app-server-control.sock")),
            &user_home,
        )?;
        let state_dir = absolute(
            config
                .state_dir
                .as_deref()
                .unwrap_or(&user_home.join(".local/state/codex-app-server-bridge")),
            &user_home,
        )?;
        if user_home.starts_with(&state_dir) || codex_home.starts_with(&state_dir) {
            bail!("state_dir must be a dedicated directory, not a home directory or its ancestor");
        }
        if let Some(port) = port {
            config.port = port;
        }
        if !config.host.is_loopback() {
            bail!("host must be a loopback address (127.0.0.1 or ::1)");
        }
        if config.poll_interval_ms < 100 || config.connect_timeout_ms < 100 {
            bail!("poll_interval_ms and connect_timeout_ms must be at least 100");
        }
        if config.max_connections == 0 || config.max_message_size == 0 {
            bail!("connection and message limits must be positive");
        }
        config.user_home = Some(user_home.clone());
        config.codex_home = Some(codex_home.clone());
        config.socket_path = Some(socket_path.clone());
        config.state_dir = Some(state_dir.clone());
        Ok(Self {
            config,
            user_home,
            codex_home,
            socket_path,
            state_dir,
        })
    }

    pub fn timeout(&self) -> Duration {
        Duration::from_millis(self.config.connect_timeout_ms)
    }
}

pub fn absolute(path: &Path, home: &Path) -> Result<PathBuf> {
    let expanded = match path.strip_prefix("~") {
        Ok(rest) => home.join(rest),
        Err(_) => path.to_path_buf(),
    };
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        std::env::current_dir()?.join(expanded)
    };
    let mut clean = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                clean.pop();
            }
            other => clean.push(other.as_os_str()),
        }
    }
    Ok(clean)
}

pub fn private_dir(path: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    Ok(())
}
