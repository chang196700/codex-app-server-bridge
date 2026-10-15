mod bridge;
mod codex_path;
mod config;
mod config_command;
mod daemon;
mod environment;
mod service;
mod socket;
mod update;
mod update_replace;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use config::Settings;
use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Parser)]
#[command(
    version,
    about = "Expose Codex app-server over a local WebSocket endpoint"
)]
struct Cli {
    /// TOML config file (default: ~/.codex-app-server-bridge/config.toml)
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Target user's home; useful when installing a system service
    #[arg(long, global = true)]
    user_home: Option<PathBuf>,
    /// Override CODEX_HOME and the TOML codex_home setting
    #[arg(long, global = true)]
    codex_home: Option<PathBuf>,
    /// Default WebSocket port only; 0 selects an available port
    #[arg(long, global = true)]
    port: Option<u16>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run in the foreground (default)
    Run,
    /// Print runtime state and whether the bridge is still running
    Status,
    /// Create an example TOML configuration (never overwrites an existing file)
    InitConfig,
    /// List, read, or update configuration options
    Config {
        #[command(subcommand)]
        action: config_command::Action,
    },
    /// Install or manage OS services / login startup
    Service {
        #[arg(value_enum)]
        action: service::Action,
        #[arg(long, value_enum)]
        scope: Option<service::Scope>,
        /// Registration backend (defaults to the native backend for --scope).
        #[arg(long = "type", value_enum)]
        service_type: Option<service::ServiceType>,
        /// Start at user login or operating-system startup.
        #[arg(long, value_enum)]
        autostart: Option<service::Autostart>,
        /// Required for Linux/macOS system services: run as this account
        #[arg(long)]
        username: Option<String>,
    },
    /// Update the installed service binary from the latest GitHub Release
    Update {
        /// Update this executable instead, without managing a service
        #[arg(long = "self", conflicts_with_all = ["scope", "username"])]
        update_self: bool,
        #[arg(long, value_enum)]
        scope: Option<service::Scope>,
        /// Required for Linux/macOS system services: the installed service account
        #[arg(long)]
        username: Option<String>,
    },
    #[cfg(windows)]
    #[command(hide = true)]
    ServiceRun,
    #[cfg(windows)]
    #[command(hide = true)]
    HiddenStart,
}

/// Write through a sibling file so readers never observe a partial document.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let temp = path.with_extension(format!("tmp-{}-{nonce}", std::process::id()));
    let mut created = false;
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        created = true;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, path)?;
        Ok::<_, anyhow::Error>(())
    })();
    if result.is_err() && created {
        let _ = std::fs::remove_file(temp);
    }
    result.with_context(|| format!("cannot write {}", path.display()))
}

fn run(
    settings: Settings,
    external_stop: Option<tokio::sync::watch::Receiver<bool>>,
) -> Result<()> {
    tokio::runtime::Builder::new_multi_thread().enable_all().build()?.block_on(async move {
        let (signal_tx, signal_rx) = tokio::sync::watch::channel(false);
        let stop = external_stop.unwrap_or(signal_rx);
        tokio::spawn(async move {
            #[cfg(unix)] {
                match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                    Ok(mut term) => { tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} } }
                    Err(error) => { tracing::warn!(%error, "SIGTERM handler unavailable"); let _ = tokio::signal::ctrl_c().await; }
                }
            }
            #[cfg(not(unix))] { let _ = tokio::signal::ctrl_c().await; }
            let _ = signal_tx.send(true);
        });
        bridge::run(settings, stop).await
    })
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Some(Command::Config { action }) = &cli.command {
        return config_command::run(action, cli.config.as_deref(), cli.user_home.as_deref());
    }
    if matches!(
        cli.command,
        Some(Command::Update {
            update_self: true,
            ..
        })
    ) {
        init_stderr_logging();
        return update::run(None);
    }
    if matches!(cli.command, Some(Command::InitConfig)) {
        let path = config::resolve_config_path(cli.config.as_deref(), cli.user_home.as_deref())?;
        if let Some(parent) = path.parent() {
            config::private_dir(parent)?;
        }
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(&path)
            .with_context(|| {
                format!(
                    "cannot create {} (existing files are never overwritten)",
                    path.display()
                )
            })?
            .write_all(config::EXAMPLE.as_bytes())?;
        println!("{}", path.display());
        return Ok(());
    }
    let mut config_path =
        config::resolve_config_path(cli.config.as_deref(), cli.user_home.as_deref())?;
    let mut settings = Settings::load(
        cli.config.as_deref(),
        cli.user_home.as_deref(),
        cli.codex_home.as_deref(),
        cli.port,
    )?;
    #[cfg(windows)]
    if matches!(cli.command, Some(Command::ServiceRun)) {
        let log_dir = settings.state_dir.join("logs");
        std::fs::create_dir_all(&log_dir)?;
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_dir.join("bridge.log"))?;
        tracing_subscriber::fmt()
            .with_writer(std::sync::Mutex::new(log))
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "info".into()),
            )
            .with_ansi(false)
            .init();
    } else {
        init_stderr_logging();
    }
    #[cfg(not(windows))]
    init_stderr_logging();
    match cli.command.unwrap_or(Command::Run) {
        Command::Run => run(settings, None),
        Command::Status => bridge::show_status(&settings),
        Command::InitConfig | Command::Config { .. } => unreachable!(),
        Command::Update {
            scope, username, ..
        } => {
            let persisted = service::load_service_config(&settings.user_home)?;
            let saved_scope = persisted.as_ref().map(|saved| saved.scope);
            if let (Some(requested), Some(saved)) = (scope, saved_scope)
                && requested != saved
            {
                anyhow::bail!(
                    "--scope {:?} does not match the installed service scope {:?}; omit --scope or reinstall the service",
                    requested,
                    saved
                );
            }
            let scope = scope.or(saved_scope).unwrap_or(service::Scope::User);
            let mut options = persisted
                .as_ref()
                .map(|saved| saved.options())
                .unwrap_or_else(|| service::ServiceOptions {
                    scope,
                    service_type: service::ServiceType::default_for_scope(scope),
                    autostart: service::ServiceType::default_for_scope(scope).default_autostart(),
                    username: None,
                    executable: service::installed_executable(&settings.user_home),
                    config_path: config_path.clone(),
                    user_home: settings.user_home.clone(),
                    codex_home: settings.codex_home.clone(),
                    state_dir: settings.state_dir.clone(),
                });
            // Command-line identity and path overrides remain authoritative
            // for system-service updates; the rest comes from service.toml.
            options.scope = scope;
            options.username = username.or(options.username);
            #[cfg(unix)]
            if matches!(options.scope, service::Scope::System)
                && (options.username.is_none()
                    || (cli.user_home.is_none() && persisted.as_ref().is_none()))
            {
                anyhow::bail!(
                    "system service updates require --username and an explicit --user-home for the target user"
                );
            }
            update::run(Some(&options))
        }
        Command::Service {
            action,
            scope,
            service_type,
            autostart,
            username: requested_username,
        } => {
            // Reuse the persisted registration when management commands omit
            // all backend options. An explicit --scope/--type always wins.
            let persisted = service::load_service_config(&settings.user_home)?;
            let explicit_scope = scope.is_some();
            let explicit_type = service_type.is_some();
            if cli.config.is_none()
                && let Some(saved) = persisted.as_ref()
                && saved.config_path != config_path
            {
                config_path = saved.config_path.clone();
                settings = Settings::load(
                    Some(&config_path),
                    Some(&saved.user_home),
                    Some(&saved.codex_home),
                    cli.port,
                )?;
            }
            let service_type = match (service_type, scope, autostart, persisted.as_ref()) {
                (Some(value), _, _, _) => value,
                (None, Some(scope), _, _) => service::ServiceType::default_for_scope(scope),
                (None, None, _, Some(saved)) => saved.service_type,
                (None, None, Some(service::Autostart::Startup), None) => {
                    service::ServiceType::default_for_scope(service::Scope::System)
                }
                (None, None, Some(service::Autostart::Login), None) | (None, None, None, None) => {
                    service::ServiceType::default_for_scope(service::Scope::User)
                }
            };
            if !service_type.supported_on_current_platform() {
                anyhow::bail!(
                    "service type {:?} is not supported on this operating system",
                    service_type
                );
            }
            if let Some(requested_scope) = scope
                && requested_scope != service_type.scope()
            {
                anyhow::bail!(
                    "--scope {:?} conflicts with --type {:?}; choose a matching service type",
                    requested_scope,
                    service_type
                );
            }
            let scope = scope.unwrap_or_else(|| service_type.scope());
            let autostart = autostart
                .or_else(|| {
                    (!explicit_scope && !explicit_type)
                        .then(|| persisted.as_ref().map(|saved| saved.autostart))
                        .flatten()
                })
                .unwrap_or_else(|| service_type.default_autostart());
            #[allow(unused_mut)]
            let mut username = requested_username
                .or_else(|| persisted.as_ref().and_then(|saved| saved.username.clone()));
            #[cfg(target_os = "macos")]
            if matches!(service_type, service::ServiceType::LaunchDaemon) && username.is_none() {
                username = Some(service::current_username()?);
            }
            #[cfg(unix)]
            if matches!(action, service::Action::Install)
                && matches!(scope, service::Scope::System)
                && (username.is_none()
                    || (cli.user_home.is_none()
                        && !matches!(service_type, service::ServiceType::LaunchDaemon)))
            {
                anyhow::bail!(
                    "system services require --username and an explicit --user-home for the target user"
                );
            }
            if matches!(action, service::Action::Install) {
                let detected_codex_path = if settings.config.transport == config::Transport::Proxy
                    || settings.config.auto_start_codex_app_server_daemon
                {
                    if settings.config.auto_start_codex_app_server_daemon {
                        daemon::check_launch_context()?;
                    }
                    Some(codex_path::resolve(
                        settings.config.codex_path.as_deref(),
                        &settings.user_home,
                    )?)
                } else {
                    None
                };
                // The stable executable always lives in the selected user's
                // default directory, even when --config selects another file.
                let default_config = config::resolve_config_path(None, Some(&settings.user_home))?;
                let install_directory = default_config
                    .parent()
                    .context("default configuration has no parent directory")?;
                #[cfg(unix)]
                if matches!(scope, service::Scope::System) {
                    service::prepare_install_directory(
                        install_directory,
                        &settings.user_home,
                        username
                            .as_deref()
                            .context("system install requires --username")?,
                    )?;
                    service::prepare_install_directory(
                        &settings.state_dir,
                        &settings.user_home,
                        username
                            .as_deref()
                            .context("system install requires --username")?,
                    )?;
                }
                config::private_dir(install_directory)?;
                config::private_dir(&settings.state_dir)?;
                let sid = settings
                    .config
                    .windows_user_sid
                    .clone()
                    .or(environment::current_user_sid()?);
                settings = config_command::prepare_service(
                    &config_path,
                    cli.user_home.as_deref(),
                    cli.codex_home.as_deref(),
                    cli.port,
                    detected_codex_path.as_deref(),
                    sid.as_deref(),
                )?;
            }
            let options = service::ServiceOptions {
                scope,
                service_type,
                autostart,
                username,
                executable: service::installed_executable(&settings.user_home),
                config_path,
                user_home: settings.user_home,
                codex_home: settings.codex_home,
                state_dir: settings.state_dir,
            };
            let result = service::manage(action, &options);
            if result.is_ok() {
                match action {
                    service::Action::Install => service::save_service_config(&options)?,
                    service::Action::Uninstall
                        if persisted.as_ref().is_none_or(|saved| {
                            saved.service_type == options.service_type
                                && saved.scope == options.scope
                        }) =>
                    {
                        service::remove_service_config(&options.user_home)?
                    }
                    _ => {}
                }
            }
            result
        }
        #[cfg(windows)]
        Command::ServiceRun => service::dispatch(move |stop| run(settings, Some(stop))),
        #[cfg(windows)]
        Command::HiddenStart => {
            use std::os::windows::process::CommandExt;
            let mut args: Vec<_> = std::env::args_os().skip(1).collect();
            for arg in &mut args {
                if arg == "hidden-start" {
                    *arg = "run".into();
                }
            }
            let log_dir = settings.state_dir.join("logs");
            std::fs::create_dir_all(&log_dir)?;
            let log = OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_dir.join("bridge.log"))?;
            std::process::Command::new(std::env::current_exe()?)
                .args(args)
                .creation_flags(0x08000000)
                .stdin(std::process::Stdio::null())
                .stdout(log.try_clone()?)
                .stderr(log)
                .spawn()?;
            Ok(())
        }
    }
}

fn init_stderr_logging() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_ansi(false)
        .init();
}
