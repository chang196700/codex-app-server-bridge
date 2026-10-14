mod bridge;
mod config;
mod config_command;
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
    about = "Monitor Codex's control socket and expose a local WebSocket endpoint"
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
    /// 0 selects an available port; any other value is a fixed port
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
        #[arg(long, value_enum, default_value = "user")]
        scope: service::Scope,
        /// Required for Linux/macOS system services: run as this account
        #[arg(long)]
        username: Option<String>,
    },
    /// Update the installed service binary from the latest GitHub Release
    Update {
        /// Update this executable instead, without managing a service
        #[arg(long = "self", conflicts_with_all = ["scope", "username"])]
        update_self: bool,
        #[arg(long, value_enum, default_value = "user")]
        scope: service::Scope,
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
            #[cfg(unix)]
            if matches!(scope, service::Scope::System)
                && (username.is_none() || cli.user_home.is_none())
            {
                anyhow::bail!(
                    "system service updates require --username and an explicit --user-home for the target user"
                );
            }
            update::run(Some(&service::ServiceOptions {
                scope,
                username,
                executable: service::installed_executable(&settings.user_home),
                config_path: settings.state_dir.join("service.toml"),
                user_home: settings.user_home,
                codex_home: settings.codex_home,
                state_dir: settings.state_dir,
            }))
        }
        Command::Service {
            action,
            scope,
            username,
        } => {
            #[cfg(unix)]
            if matches!(action, service::Action::Install)
                && matches!(scope, service::Scope::System)
                && (username.is_none() || cli.user_home.is_none())
            {
                anyhow::bail!(
                    "system services require --username and an explicit --user-home for the target user"
                );
            }
            let snapshot = settings.state_dir.join("service.toml");
            if matches!(action, service::Action::Install) {
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
                if settings.config.windows_user_sid.is_none() {
                    settings.config.windows_user_sid = environment::current_user_sid()?;
                }
                write_atomic(
                    &snapshot,
                    toml::to_string_pretty(&settings.config)?.as_bytes(),
                )?;
            }
            let options = service::ServiceOptions {
                scope,
                username,
                executable: service::installed_executable(&settings.user_home),
                config_path: snapshot,
                user_home: settings.user_home,
                codex_home: settings.codex_home,
                state_dir: settings.state_dir,
            };
            service::manage(action, &options)
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
