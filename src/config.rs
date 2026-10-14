use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, Metadata, OpenOptions},
    io::{ErrorKind, Write},
    net::IpAddr,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const EXAMPLE: &str = r#"# ~/.codex-app-server-bridge/config.toml
# Defaults to $CODEX_HOME, otherwise ~/.codex.
# codex_home = "~/.codex"
# socket_path = "~/.codex/app-server-control/app-server-control.sock"
# Opt in to starting the Codex app-server daemon when it is not running.
auto_start_codex_app_server_daemon = false
# Optional Codex executable; otherwise discover it from PATH/common install locations.
# codex_path = "/absolute/path/to/codex"

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
    pub codex_path: Option<PathBuf>,
    pub auto_start_codex_app_server_daemon: bool,
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
            codex_path: None,
            auto_start_codex_app_server_daemon: false,
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

impl Config {
    /// Validate the document itself, without applying runtime environment or
    /// command-line overrides and without writing normalized values back to it.
    pub fn validate(&self, user_home: Option<&Path>) -> Result<()> {
        Settings::from_config(self.clone(), user_home, None, None, false).map(|_| ())
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
        let path = resolve_config_path(config_path, user_home)?;
        let config: Config = match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text)
                .with_context(|| format!("invalid TOML in {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && config_path.is_none() => {
                Config::default()
            }
            Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
        };
        Self::from_config(config, user_home, codex_home, port, true)
    }

    pub fn from_config(
        mut config: Config,
        user_home: Option<&Path>,
        codex_home: Option<&Path>,
        port: Option<u16>,
        use_environment: bool,
    ) -> Result<Self> {
        let default_home = home_directory(user_home)?;
        let user_home = absolute(
            user_home
                .or(config.user_home.as_deref())
                .unwrap_or(&default_home),
            &default_home,
        )?;
        let env_codex = use_environment
            .then(|| std::env::var_os("CODEX_HOME"))
            .flatten()
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        let codex_home = absolute(
            codex_home
                .or(config.codex_home.as_deref())
                .or(env_codex.as_deref())
                .unwrap_or(&user_home.join(".codex")),
            &user_home,
        )?;
        if let Some(path) = &config.codex_path {
            if path.as_os_str().is_empty() {
                bail!(
                    "codex_path must be a nonempty executable path; omit it for automatic detection"
                );
            }
            config.codex_path = Some(absolute(path, &user_home)?);
        }
        let socket_path = absolute(
            config
                .socket_path
                .as_deref()
                .unwrap_or(&codex_home.join("app-server-control/app-server-control.sock")),
            &user_home,
        )?;
        if config.auto_start_codex_app_server_daemon
            && socket_path != codex_home.join("app-server-control/app-server-control.sock")
        {
            bail!(
                "automatic daemon startup requires the default control socket under codex_home; remove the socket_path override"
            );
        }
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
        if let Some(sid) = &config.windows_user_sid
            && (!sid.starts_with("S-1-")
                || sid[4..]
                    .split('-')
                    .any(|part| part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit())))
        {
            bail!("windows_user_sid must be a Windows SID such as S-1-5-21-...");
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

const CONFIG_DIRECTORY: &str = ".codex-app-server-bridge";
const MIGRATION_LOCK: &str = ".codex-app-server-bridge.migration.lock";
const MIGRATION_PENDING: &str = ".codex-app-server-bridge.migration.pending";
const BACKUP_PREFIX: &str = ".codex-app-server-bridge.legacy-";

pub fn home_directory(user_home: Option<&Path>) -> Result<PathBuf> {
    let detected = dirs::home_dir();
    let home = user_home
        .or(detected.as_deref())
        .context("cannot determine user home; pass --user-home")?;
    absolute(home, detected.as_deref().unwrap_or(home))
}

/// Resolve the default directory and migrate a legacy TOML file if necessary.
/// A missing directory is returned without creating it. The legacy backup is
/// retained beside this directory after a successful migration.
pub fn default_directory(home: &Path) -> Result<PathBuf> {
    let home = home_directory(Some(home))?;
    let directory = home.join(CONFIG_DIRECTORY);
    let pending = home.join(MIGRATION_PENDING);
    let metadata = regular_metadata(&directory, true)?;
    if metadata.as_ref().is_some_and(Metadata::is_file) || pending.exists() {
        let _lock = lock_file(&home.join(MIGRATION_LOCK), &home)?;
        migrate_legacy(&home, &directory)?;
    }
    Ok(directory)
}

pub fn resolve_config_path(explicit: Option<&Path>, user_home: Option<&Path>) -> Result<PathBuf> {
    let home = home_directory(user_home)?;
    let legacy = home.join(CONFIG_DIRECTORY);
    let default = legacy.join("config.toml");
    let selected = explicit.map(|path| absolute(path, &home)).transpose()?;
    if selected
        .as_ref()
        .is_none_or(|path| path == &legacy || path == &default)
    {
        Ok(default_directory(&home)?.join("config.toml"))
    } else {
        Ok(selected.expect("explicit path was checked"))
    }
}

/// Locks the read-modify-write operation used by `config set`. Atomic replace
/// alone would allow concurrent commands to silently overwrite each other's keys.
pub fn lock_configuration(path: &Path, home: &Path) -> Result<File> {
    let parent = path.parent().context("configuration path has no parent")?;
    ensure_owned_directory(parent, home)?;
    let filename = path
        .file_name()
        .context("configuration path has no filename")?;
    let mut lock_name = filename.to_os_string();
    lock_name.push(".lock");
    lock_file(&parent.join(lock_name), home)
}

/// Atomically save an edited document, preserving the previous file's Unix
/// mode/owner. Newly created files belong to the selected user's home owner.
pub fn write_configuration(path: &Path, bytes: &[u8], home: &Path) -> Result<()> {
    let previous = regular_metadata(path, false)?;
    let parent = path.parent().context("configuration path has no parent")?;
    ensure_owned_directory(parent, home)?;
    atomic_document(path, bytes, previous.as_ref(), home)
}

fn regular_metadata(path: &Path, allow_directory: bool) -> Result<Option<Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!(
                "refusing to replace or migrate symlink {}; use its real target explicitly",
                path.display()
            );
        }
        Ok(metadata) if metadata.is_file() || allow_directory && metadata.is_dir() => {
            Ok(Some(metadata))
        }
        Ok(_) => bail!(
            "{} is not a regular configuration file{}",
            path.display(),
            if allow_directory { " or directory" } else { "" }
        ),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("inspect {}", path.display())),
    }
}

fn preserve_owner(path: &Path, reference: &Metadata) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, chown};
        let actual = fs::metadata(path)?;
        if actual.uid() != reference.uid() || actual.gid() != reference.gid() {
            chown(path, Some(reference.uid()), Some(reference.gid()))
                .with_context(|| format!("preserve configuration owner on {}", path.display()))?;
        }
    }
    #[cfg(not(unix))]
    let _ = (path, reference);
    Ok(())
}

fn ensure_owned_directory(path: &Path, home: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => return Ok(()),
        Ok(_) => bail!(
            "configuration directory {} is not a regular directory",
            path.display()
        ),
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
    }
    // Create missing ancestors one at a time so sudo never leaves a root-owned
    // 0700 ancestor preventing the selected user from reaching their config.
    if let Some(parent) = path.parent()
        && !parent.is_dir()
    {
        ensure_owned_directory(parent, home)?;
    }
    let builder = fs::DirBuilder::new();
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = builder;
        builder.mode(0o700);
        builder
    };
    match builder.create(path) {
        Ok(()) => {
            if let Ok(owner) = fs::metadata(home) {
                preserve_owner(path, &owner)?;
            }
        }
        Err(error) if error.kind() == ErrorKind::AlreadyExists && path.is_dir() => {}
        Err(error) => return Err(error).with_context(|| format!("create {}", path.display())),
    }
    Ok(())
}

fn new_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).read(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .with_context(|| format!("create {}", path.display()))
}

fn lock_file(path: &Path, home: &Path) -> Result<File> {
    regular_metadata(path, false)?;
    let file = match new_file(path) {
        Ok(file) => {
            if let Ok(owner) = fs::metadata(home) {
                preserve_owner(path, &owner)?;
            }
            file
        }
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == ErrorKind::AlreadyExists) =>
        {
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .with_context(|| format!("open configuration lock {}", path.display()))?
        }
        Err(error) => return Err(error),
    };
    fs2::FileExt::lock_exclusive(&file)
        .with_context(|| format!("lock configuration {}", path.display()))?;
    Ok(file)
}

fn write_new_document(
    path: &Path,
    bytes: &[u8],
    original: Option<&Metadata>,
    home: &Path,
) -> Result<()> {
    let mut file = new_file(path)?;
    file.write_all(bytes)?;
    if let Some(original) = original {
        preserve_owner(path, original)?;
        file.set_permissions(original.permissions())?;
    } else if let Ok(owner) = fs::metadata(home) {
        preserve_owner(path, &owner)?;
    }
    file.sync_all()?;
    Ok(())
}

fn unique_suffix() -> Result<String> {
    Ok(format!(
        "{}-{}",
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
        std::process::id()
    ))
}

fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn atomic_document(
    path: &Path,
    bytes: &[u8],
    original: Option<&Metadata>,
    home: &Path,
) -> Result<()> {
    let parent = path.parent().context("configuration path has no parent")?;
    let temporary = parent.join(format!(".config-{}.tmp", unique_suffix()?));
    let result = (|| {
        write_new_document(&temporary, bytes, original, home)?;
        fs::rename(&temporary, path)?;
        sync_directory(parent)?;
        Ok::<_, anyhow::Error>(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.with_context(|| format!("save configuration {}", path.display()))
}

fn migrate_legacy(home: &Path, directory: &Path) -> Result<()> {
    let pending = home.join(MIGRATION_PENDING);
    let backup = if regular_metadata(&pending, false)?.is_some() {
        let filename = fs::read_to_string(&pending)?;
        let filename = filename.trim_end_matches('\n');
        if !filename.starts_with(BACKUP_PREFIX)
            || !filename.ends_with(".toml.bak")
            || filename.contains(['/', '\\', '\r', '\n'])
        {
            bail!(
                "invalid migration record {}; original configuration has not been changed",
                pending.display()
            );
        }
        home.join(filename)
    } else {
        let Some(metadata) = regular_metadata(directory, true)? else {
            return Ok(());
        };
        if metadata.is_dir() {
            return Ok(());
        }
        let bytes = fs::read(directory)?;
        // Do not transform an unrelated file into our configuration directory.
        let text = std::str::from_utf8(&bytes).context("legacy configuration is not UTF-8 TOML")?;
        toml::from_str::<toml::Value>(text).context("legacy configuration is not valid TOML")?;
        let filename = format!("{BACKUP_PREFIX}{}.toml.bak", unique_suffix()?);
        let backup = home.join(&filename);
        write_new_document(&backup, &bytes, Some(&metadata), home)?;
        sync_directory(home)?;
        // Publish recovery information before changing the old path. Both the
        // immutable backup and this record survive any subsequent failure.
        atomic_document(&pending, format!("{filename}\n").as_bytes(), None, home)?;
        sync_directory(home)?;
        backup
    };
    let result = finish_migration(home, directory, &backup, &pending);
    result.with_context(|| format!(
        "configuration migration is recoverable: original bytes are in {}; retry to resume using {}",
        backup.display(), pending.display()
    ))
}

fn finish_migration(home: &Path, directory: &Path, backup: &Path, pending: &Path) -> Result<()> {
    let metadata = regular_metadata(backup, false)?.context("migration backup is missing")?;
    let bytes = fs::read(backup)?;
    if let Some(current) = regular_metadata(directory, true)?
        && current.is_file()
    {
        if fs::read(directory)? != bytes {
            bail!(
                "legacy configuration changed during migration; preserving both the current file and its backup"
            );
        }
        fs::remove_file(directory)?;
        sync_directory(home)?;
    }
    ensure_owned_directory(directory, home)?;
    // A previous privileged attempt can have stopped between mkdir and chown.
    // This recovery path owns the dedicated default configuration directory;
    // never apply this correction to arbitrary existing --config parents.
    preserve_owner(directory, &fs::metadata(home)?)?;
    let destination = directory.join("config.toml");
    if regular_metadata(&destination, false)?.is_some() {
        if fs::read(&destination)? != bytes {
            bail!(
                "configuration changed during migration recovery; preserving both config.toml and its backup"
            );
        }
    } else {
        atomic_document(&destination, &bytes, Some(&metadata), home)?;
    }
    fs::remove_file(pending)?;
    sync_directory(home)?;
    tracing::info!(config = %destination.display(), backup = %backup.display(), "migrated legacy TOML configuration");
    Ok(())
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
