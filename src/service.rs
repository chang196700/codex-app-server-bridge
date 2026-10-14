//! Installation and lifecycle management for operating-system startup services.

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use std::path::{Path, PathBuf};

#[cfg(not(target_os = "macos"))]
const SERVICE_NAME: &str = "codex-app-server-bridge";
#[cfg(unix)]
const MANAGED_MARKER: &str = "Managed by codex-app-server-bridge";

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum Scope {
    User,
    System,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum Action {
    Install,
    Uninstall,
    Start,
    Stop,
    Status,
}

#[derive(Clone, Debug)]
pub struct ServiceOptions {
    pub scope: Scope,
    pub username: Option<String>,
    pub executable: PathBuf,
    pub config_path: PathBuf,
    pub user_home: PathBuf,
    pub codex_home: PathBuf,
    pub state_dir: PathBuf,
}

pub fn manage(action: Action, options: &ServiceOptions) -> Result<()> {
    for path in [
        &options.executable,
        &options.config_path,
        &options.user_home,
        &options.codex_home,
        &options.state_dir,
    ] {
        if !path.is_absolute() {
            bail!("service paths must be absolute: {}", path.display());
        }
        // Services serialize paths to UTF-8 configuration files and command lines.
        path_text(path)?;
    }
    if action == Action::Install {
        if !options.executable.is_file() || !options.config_path.is_file() {
            bail!("service executable and configuration snapshot must exist before installation");
        }
        std::fs::create_dir_all(&options.state_dir).context("create service state directory")?;
    }
    platform::manage(action, options)
}

fn path_text(path: &Path) -> Result<&str> {
    let value = path
        .to_str()
        .context("service paths must be valid Unicode")?;
    if value.chars().any(char::is_control) {
        bail!("service paths must not contain control characters");
    }
    Ok(value)
}

fn arguments(options: &ServiceOptions, subcommand: &str) -> Vec<std::ffi::OsString> {
    // Installation snapshots pin the target user's paths and identity. Keeping
    // only the snapshot argument also fits Windows' limited Run-key command.
    vec![
        "--config".into(),
        options.config_path.as_os_str().to_owned(),
        subcommand.into(),
    ]
}

#[cfg(any(windows, target_os = "macos"))]
fn runtime_running(options: &ServiceOptions) -> Result<bool> {
    let lock = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(options.state_dir.join("bridge.lock"))
    {
        Ok(lock) => lock,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).context("open runtime lock"),
    };
    match fs2::FileExt::try_lock_exclusive(&lock) {
        Ok(()) => {
            fs2::FileExt::unlock(&lock)?;
            Ok(false)
        }
        Err(error) if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
            Ok(true)
        }
        Err(error) => Err(error).context("inspect runtime lock"),
    }
}

#[cfg(unix)]
fn checked(command: &mut std::process::Command) -> Result<std::process::Output> {
    let description = format!("{command:?}");
    let output = command
        .output()
        .with_context(|| format!("execute {description}"))?;
    if !output.status.success() {
        bail!(
            "{description} failed ({}): {}{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(output)
}

#[cfg(unix)]
fn check_managed(path: &Path) -> Result<()> {
    match std::fs::read_to_string(path) {
        Ok(content) if !content.contains(MANAGED_MARKER) => {
            bail!(
                "refusing to modify an unmanaged service file: {}",
                path.display()
            );
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

#[cfg(unix)]
fn write_managed(path: &Path, content: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    check_managed(path)?;
    let parent = path
        .parent()
        .context("service file has no parent directory")?;
    std::fs::create_dir_all(parent)?;
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut created = false;
    let result = (|| -> Result<()> {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        created = true;
        file.set_permissions(std::fs::Permissions::from_mode(0o644))?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() && created {
        let _ = std::fs::remove_file(&temporary);
    }
    result.with_context(|| format!("write service file {}", path.display()))
}

#[cfg(unix)]
fn user_id(username: &str) -> Result<u32> {
    if username.is_empty()
        || username.starts_with('-')
        || username
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
    {
        bail!("invalid service username");
    }
    let output = checked(std::process::Command::new("id").args(["-u", username]))?;
    String::from_utf8(output.stdout)?
        .trim()
        .parse()
        .context("id returned an invalid user ID")
}

/// Prepare an elevated installation before the snapshot is written. Existing
/// parent directories retain their owner and permissions; only parents created
/// here inside the target home are assigned to that user.
#[cfg(unix)]
pub fn prepare_install_directory(state_dir: &Path, user_home: &Path, username: &str) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    if !state_dir.is_absolute() || !user_home.is_absolute() {
        bail!("system installation requires absolute home and state directory paths");
    }
    if state_dir
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        bail!("state_dir must not contain unresolved parent-directory components");
    }
    let home = std::fs::canonicalize(user_home).context("target user's home must already exist")?;
    if !home.is_dir() {
        bail!("target user's home is not a directory");
    }
    let uid = user_id(username)?.to_string();
    let mut missing = Vec::new();
    let mut ancestor = state_dir;
    loop {
        match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                if ancestor == state_dir && metadata.file_type().is_symlink() {
                    bail!("system service state directory must not be a symbolic link");
                }
                if !std::fs::metadata(ancestor)?.is_dir() {
                    bail!(
                        "state directory ancestor is not a directory: {}",
                        ancestor.display()
                    );
                }
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(ancestor.to_path_buf());
                ancestor = ancestor
                    .parent()
                    .context("state directory has no existing ancestor")?;
            }
            Err(error) => return Err(error).context("inspect state directory ancestry"),
        }
    }
    let ancestor_target = std::fs::canonicalize(ancestor)?;
    let target = ancestor_target.join(state_dir.strip_prefix(ancestor)?);
    if home.starts_with(&target) {
        bail!("state_dir must be a dedicated directory, not the user's home or its ancestor");
    }
    for directory in missing.iter().rev() {
        let resolved = ancestor_target.join(directory.strip_prefix(ancestor)?);
        // Explicitly fix the mode on newly created directories so an elevated
        // installer's umask cannot make an outside-home parent untraversable.
        std::fs::DirBuilder::new()
            .mode(0o755)
            .create(directory)
            .with_context(|| format!("create {}", directory.display()))?;
        if resolved.starts_with(&home) && directory != state_dir {
            checked(std::process::Command::new("chown").arg(&uid).arg(directory))?;
        }
        let mode = if resolved.starts_with(&home) {
            0o700
        } else {
            0o755
        };
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(mode))?;
    }
    checked(std::process::Command::new("chown").arg(&uid).arg(state_dir))?;
    std::fs::set_permissions(state_dir, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(unix)]
fn system_username(options: &ServiceOptions) -> Result<&str> {
    let username = options.username.as_deref().context(
        "system installation requires --username for the user who owns the Codex socket",
    )?;
    user_id(username)?;
    Ok(username)
}

#[cfg(unix)]
fn prepare_system_files(options: &ServiceOptions, username: &str) -> Result<()> {
    // These are the two artifacts an elevated installer creates. Do not chown
    // recursively: other user data and socket permissions belong to their owners.
    if std::fs::symlink_metadata(&options.state_dir)?
        .file_type()
        .is_symlink()
        || std::fs::symlink_metadata(&options.config_path)?
            .file_type()
            .is_symlink()
    {
        bail!("system service state directory and snapshot must not be symbolic links");
    }
    let directory = std::fs::canonicalize(&options.state_dir)?;
    if directory.parent().is_none()
        || directory == std::fs::canonicalize(&options.user_home)?
        || std::fs::canonicalize(&options.codex_home).is_ok_and(|home| directory == home)
    {
        bail!("system service state_dir must be a dedicated runtime directory");
    }
    checked(
        std::process::Command::new("chown")
            .arg(username)
            .arg(&options.state_dir)
            .arg(&options.config_path),
    )?;
    Ok(())
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use std::process::Command;

    fn quote(value: &str, exec: bool) -> String {
        let escaped = value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%");
        format!(
            "\"{}\"",
            if exec {
                escaped.replace('$', "$$")
            } else {
                escaped
            }
        )
    }

    fn unit_path(options: &ServiceOptions) -> PathBuf {
        let directory = match options.scope {
            Scope::User => options.user_home.join(".config/systemd/user"),
            Scope::System => PathBuf::from("/etc/systemd/system"),
        };
        directory.join(format!("{SERVICE_NAME}.service"))
    }

    fn systemctl(scope: Scope) -> Command {
        let mut command = Command::new("systemctl");
        if scope == Scope::User {
            command.arg("--user");
        }
        command
    }

    pub fn manage(action: Action, options: &ServiceOptions) -> Result<()> {
        let path = unit_path(options);
        let name = format!("{SERVICE_NAME}.service");
        match action {
            Action::Install => {
                let identity = if options.scope == Scope::System {
                    let username = system_username(options)?;
                    prepare_system_files(options, username)?;
                    format!("User={}\n", quote(username, false))
                } else {
                    String::new()
                };
                let mut command = vec![quote(path_text(&options.executable)?, true)];
                command.extend(arguments(options, "run").iter().map(|arg| {
                    quote(
                        arg.to_str().expect("validated Unicode service arguments"),
                        true,
                    )
                }));
                let target = if options.scope == Scope::User {
                    "default.target"
                } else {
                    "multi-user.target"
                };
                let content = format!(
                    "# {MANAGED_MARKER}\n[Unit]\nDescription=Codex app-server WebSocket bridge\nAfter=network.target\n\n[Service]\nType=exec\n{identity}ExecStart={}\nWorkingDirectory={}\nEnvironment={}\nEnvironment={}\nRestart=on-failure\nRestartSec=3\nTimeoutStopSec=30\nUMask=0077\n\n[Install]\nWantedBy={target}\n",
                    command.join(" "),
                    quote(path_text(&options.user_home)?, false),
                    quote(&format!("HOME={}", path_text(&options.user_home)?), false),
                    quote(
                        &format!("CODEX_HOME={}", path_text(&options.codex_home)?),
                        false
                    ),
                );
                write_managed(&path, &content)?;
                checked(systemctl(options.scope).arg("daemon-reload"))?;
                checked(systemctl(options.scope).args(["enable", &name]))?;
                checked(systemctl(options.scope).args(["restart", &name]))?;
                println!("Installed and started {}", path.display());
                if options.scope == Scope::User {
                    println!(
                        "For startup before login, enable lingering for this user with loginctl enable-linger."
                    );
                }
            }
            Action::Uninstall => {
                check_managed(&path)?;
                if path.exists() {
                    checked(systemctl(options.scope).args(["disable", "--now", &name]))?;
                    std::fs::remove_file(&path)?;
                    checked(systemctl(options.scope).arg("daemon-reload"))?;
                }
                println!("Uninstalled {name}");
            }
            Action::Start | Action::Stop | Action::Status => {
                let verb = match action {
                    Action::Start => "start",
                    Action::Stop => "stop",
                    _ => "status",
                };
                let output = checked(systemctl(options.scope).args([verb, "--no-pager", &name]))?;
                print!("{}", String::from_utf8_lossy(&output.stdout));
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use std::process::Command;

    const LABEL: &str = "io.codex.app-server-bridge";

    fn xml(value: &str) -> String {
        value
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;")
    }

    fn domain(scope: Scope) -> Result<String> {
        if scope == Scope::System {
            return Ok("system".into());
        }
        let output = checked(Command::new("id").arg("-u"))?;
        let uid: u32 = String::from_utf8(output.stdout)?.trim().parse()?;
        Ok(format!("gui/{uid}"))
    }

    fn loaded(target: &str) -> Result<bool> {
        let output = Command::new("launchctl").args(["print", target]).output()?;
        if output.status.success() {
            return Ok(true);
        }
        // launchctl uses ESRCH / its service-not-found status for absent jobs.
        // Other failures (including permissions) must not look like absence.
        if matches!(output.status.code(), Some(3 | 113)) {
            return Ok(false);
        }
        bail!(
            "launchctl print {target} failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )
    }

    fn bootout(target: &str, options: &ServiceOptions) -> Result<()> {
        checked(Command::new("launchctl").args(["bootout", target]))?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while runtime_running(options)? {
            if std::time::Instant::now() >= deadline {
                bail!(
                    "launchd unloaded {target}, but the bridge still holds its runtime lock after 30 seconds"
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        Ok(())
    }

    pub fn manage(action: Action, options: &ServiceOptions) -> Result<()> {
        let directory = match options.scope {
            Scope::User => options.user_home.join("Library/LaunchAgents"),
            Scope::System => PathBuf::from("/Library/LaunchDaemons"),
        };
        let path = directory.join(format!("{LABEL}.plist"));
        let domain = domain(options.scope)?;
        let target = format!("{domain}/{LABEL}");
        match action {
            Action::Install => {
                let identity = if options.scope == Scope::System {
                    let username = system_username(options)?;
                    prepare_system_files(options, username)?;
                    format!("<key>UserName</key><string>{}</string>\n", xml(username))
                } else {
                    String::new()
                };
                let mut args = vec![options.executable.as_os_str().to_owned()];
                args.extend(arguments(options, "run"));
                let args = args
                    .iter()
                    .map(|arg| {
                        format!(
                            "<string>{}</string>",
                            xml(arg.to_str().expect("validated Unicode service arguments"))
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let content = format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<!-- {MANAGED_MARKER} -->\n<plist version=\"1.0\"><dict>\n<key>Label</key><string>{LABEL}</string>\n<key>ProgramArguments</key><array>\n{args}\n</array>\n{identity}<key>WorkingDirectory</key><string>{}</string>\n<key>EnvironmentVariables</key><dict><key>HOME</key><string>{}</string><key>CODEX_HOME</key><string>{}</string></dict>\n<key>RunAtLoad</key><true/>\n<key>KeepAlive</key><true/>\n<key>ThrottleInterval</key><integer>3</integer>\n<key>ExitTimeOut</key><integer>30</integer>\n<key>StandardOutPath</key><string>{}</string>\n<key>StandardErrorPath</key><string>{}</string>\n</dict></plist>\n",
                    xml(path_text(&options.user_home)?),
                    xml(path_text(&options.user_home)?),
                    xml(path_text(&options.codex_home)?),
                    xml(path_text(&options.state_dir.join("service.stdout.log"))?),
                    xml(path_text(&options.state_dir.join("service.stderr.log"))?),
                );
                write_managed(&path, &content)?;
                if loaded(&target)? {
                    bootout(&target, options)?;
                }
                checked(Command::new("launchctl").args(["enable", &target]))?;
                checked(
                    Command::new("launchctl")
                        .arg("bootstrap")
                        .arg(&domain)
                        .arg(&path),
                )?;
                println!("Installed and started {}", path.display());
            }
            Action::Uninstall => {
                check_managed(&path)?;
                if loaded(&target)? {
                    bootout(&target, options)?;
                }
                if path.exists() {
                    std::fs::remove_file(&path)?;
                }
                println!("Uninstalled {LABEL}");
            }
            Action::Start => {
                if !loaded(&target)? {
                    checked(
                        Command::new("launchctl")
                            .arg("bootstrap")
                            .arg(&domain)
                            .arg(&path),
                    )?;
                }
                checked(Command::new("launchctl").args(["kickstart", &target]))?;
            }
            Action::Stop => {
                if loaded(&target)? {
                    bootout(&target, options)?;
                }
            }
            Action::Status => {
                let output = checked(Command::new("launchctl").args(["print", &target]))?;
                print!("{}", String::from_utf8_lossy(&output.stdout));
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
pub use platform::dispatch;

#[cfg(windows)]
mod platform {
    use super::*;
    use std::ffi::{OsStr, OsString};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    use tokio::sync::watch;
    use windows_service::service::{
        Service, ServiceAccess, ServiceAction, ServiceActionType, ServiceControl,
        ServiceControlAccept, ServiceErrorControl, ServiceExitCode, ServiceFailureActions,
        ServiceFailureResetPeriod, ServiceInfo, ServiceStartType, ServiceState, ServiceStatus,
        ServiceType,
    };
    use windows_service::service_control_handler::{
        self, ServiceControlHandlerResult, ServiceStatusHandle,
    };
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    type Runner = Box<dyn FnOnce(watch::Receiver<bool>) -> Result<()> + Send>;
    static RUNNER: Mutex<Option<Runner>> = Mutex::new(None);
    static SERVICE_RESULT: Mutex<Option<Result<()>>> = Mutex::new(None);

    windows_service::define_windows_service!(ffi_service_main, service_main);

    pub fn dispatch<F>(run: F) -> Result<()>
    where
        F: FnOnce(watch::Receiver<bool>) -> Result<()> + Send + 'static,
    {
        let mut slot = RUNNER
            .lock()
            .map_err(|_| anyhow::anyhow!("service runner lock poisoned"))?;
        if slot.is_some() {
            bail!("Windows service dispatcher is already active");
        }
        *slot = Some(Box::new(run));
        drop(slot);
        windows_service::service_dispatcher::start(SERVICE_NAME, ffi_service_main).context(
            "connect to Windows Service Control Manager; use `run` for foreground execution",
        )?;
        SERVICE_RESULT
            .lock()
            .map_err(|_| anyhow::anyhow!("service result lock poisoned"))?
            .take()
            .context("Windows service entry point did not complete")?
    }

    fn service_status(state: ServiceState, failed: bool) -> ServiceStatus {
        ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: if state == ServiceState::Running {
                ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
            } else {
                ServiceControlAccept::empty()
            },
            exit_code: if failed {
                ServiceExitCode::ServiceSpecific(1)
            } else {
                ServiceExitCode::Win32(0)
            },
            checkpoint: if state == ServiceState::StopPending {
                1
            } else {
                0
            },
            wait_hint: if state == ServiceState::StopPending {
                Duration::from_secs(30)
            } else {
                Duration::ZERO
            },
            process_id: None,
        }
    }

    fn service_main(_: Vec<OsString>) {
        let result = run_service();
        if let Err(error) = &result {
            eprintln!("Windows service failed: {error:#}");
        }
        if let Ok(mut slot) = SERVICE_RESULT.lock() {
            *slot = Some(result);
        }
    }

    fn run_service() -> Result<()> {
        let (stop, receiver) = watch::channel(false);
        let shared_handle: Arc<Mutex<Option<ServiceStatusHandle>>> = Arc::new(Mutex::new(None));
        let handler_handle = Arc::clone(&shared_handle);
        let handle = service_control_handler::register(SERVICE_NAME, move |event| match event {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                if let Ok(slot) = handler_handle.lock()
                    && let Some(handle) = slot.as_ref()
                {
                    let _ =
                        handle.set_service_status(service_status(ServiceState::StopPending, false));
                }
                let _ = stop.send(true);
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        })?;
        *shared_handle
            .lock()
            .map_err(|_| anyhow::anyhow!("service status lock poisoned"))? = Some(handle);
        let run = RUNNER
            .lock()
            .map_err(|_| anyhow::anyhow!("service runner lock poisoned"))?
            .take()
            .context("missing Windows service runner")?;
        handle.set_service_status(service_status(ServiceState::Running, false))?;
        let result = run(receiver);
        handle.set_service_status(service_status(ServiceState::Stopped, result.is_err()))?;
        result
    }

    pub fn manage(action: Action, options: &ServiceOptions) -> Result<()> {
        if options.username.is_some() {
            bail!("--username is only supported for Linux and macOS system services");
        }
        match options.scope {
            Scope::User => manage_autostart(action, options),
            Scope::System => manage_system(action, options),
        }
    }

    fn wait_for(service: &Service, desired: ServiceState) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let status = service.query_status()?;
            if status.current_state == desired {
                return Ok(());
            }
            if desired == ServiceState::Running && status.current_state == ServiceState::Stopped {
                bail!(
                    "service exited before reaching Running: {:?}",
                    status.exit_code
                );
            }
            if Instant::now() >= deadline {
                bail!(
                    "timed out waiting for service to become {desired:?}; current state: {:?}",
                    status.current_state
                );
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    fn stop_service(service: &Service) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut status = service.query_status()?;
        while status.current_state == ServiceState::StartPending {
            if Instant::now() >= deadline {
                bail!(
                    "service remained StartPending for 30 seconds and cannot yet accept stop requests"
                );
            }
            std::thread::sleep(Duration::from_millis(200));
            status = service.query_status()?;
        }
        if status.current_state == ServiceState::Stopped {
            return Ok(());
        }
        if status.current_state != ServiceState::StopPending {
            service.stop()?;
        }
        wait_for(service, ServiceState::Stopped)
    }

    fn missing_service(error: &windows_service::Error) -> bool {
        matches!(error, windows_service::Error::Winapi(error) if error.raw_os_error() == Some(1060))
    }

    fn manage_system(action: Action, options: &ServiceOptions) -> Result<()> {
        let manager_access = if action == Action::Install {
            ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE
        } else {
            ServiceManagerAccess::CONNECT
        };
        let manager = ServiceManager::local_computer(None::<&str>, manager_access)
            .context("open Windows Service Control Manager (system changes require elevation)")?;
        if action == Action::Install {
            let info = ServiceInfo {
                name: SERVICE_NAME.into(),
                display_name: "Codex app-server WebSocket bridge".into(),
                service_type: ServiceType::OWN_PROCESS,
                start_type: ServiceStartType::AutoStart,
                error_control: ServiceErrorControl::Normal,
                executable_path: options.executable.clone(),
                launch_arguments: arguments(options, "service-run"),
                dependencies: vec![],
                account_name: None,
                account_password: None,
            };
            let access = ServiceAccess::QUERY_STATUS
                | ServiceAccess::CHANGE_CONFIG
                | ServiceAccess::START
                | ServiceAccess::STOP;
            let service = match manager.open_service(SERVICE_NAME, access) {
                Ok(service) => {
                    stop_service(&service)?;
                    service.change_config(&info)?;
                    service
                }
                Err(error) if missing_service(&error) => manager.create_service(&info, access)?,
                Err(error) => return Err(error.into()),
            };
            service.set_description(
                "Bridges the Codex app-server control socket to a local WebSocket endpoint.",
            )?;
            service.set_delayed_auto_start(true)?;
            service.update_failure_actions(ServiceFailureActions {
                reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(86400)),
                reboot_msg: None,
                command: None,
                actions: Some(vec![ServiceAction {
                    action_type: ServiceActionType::Restart,
                    delay: Duration::from_secs(5),
                }]),
            })?;
            service.set_failure_actions_on_non_crash_failures(true)?;
            service.start::<&OsStr>(&[])?;
            wait_for(&service, ServiceState::Running)?;
            println!("Installed and started Windows service {SERVICE_NAME}");
            return Ok(());
        }
        let access = match action {
            Action::Uninstall => {
                ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE
            }
            Action::Start => ServiceAccess::QUERY_STATUS | ServiceAccess::START,
            Action::Stop => ServiceAccess::QUERY_STATUS | ServiceAccess::STOP,
            _ => ServiceAccess::QUERY_STATUS,
        };
        let service = match manager.open_service(SERVICE_NAME, access) {
            Ok(service) => service,
            Err(error) if action == Action::Uninstall && missing_service(&error) => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        match action {
            Action::Uninstall => {
                stop_service(&service)?;
                service.delete()?;
                drop(service);
                let deadline = Instant::now() + Duration::from_secs(30);
                loop {
                    match manager.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS) {
                        Err(error) if missing_service(&error) => break,
                        Err(windows_service::Error::Winapi(error))
                            if error.raw_os_error() == Some(1072) => {}
                        Err(error) => return Err(error.into()),
                        Ok(_) => {}
                    }
                    if Instant::now() >= deadline {
                        bail!(
                            "service is stopped and marked for deletion, but another open SCM handle prevents removal"
                        );
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
                println!("Uninstalled Windows service {SERVICE_NAME}");
            }
            Action::Start => {
                let state = service.query_status()?.current_state;
                if state == ServiceState::StartPending {
                    wait_for(&service, ServiceState::Running)?;
                } else if state != ServiceState::Running {
                    if state == ServiceState::StopPending {
                        wait_for(&service, ServiceState::Stopped)?;
                    }
                    service.start::<&OsStr>(&[])?;
                    wait_for(&service, ServiceState::Running)?;
                }
            }
            Action::Stop => stop_service(&service)?,
            Action::Status => println!("{:?}", service.query_status()?),
            Action::Install => unreachable!(),
        }
        Ok(())
    }

    // Windows command-line quoting follows CommandLineToArgvW/CRT rules, including
    // backslashes preceding quotes and the closing quote.
    fn quote_argument(value: &str) -> String {
        let mut quoted = String::from("\"");
        let mut backslashes = 0;
        for character in value.chars() {
            if character == '\\' {
                backslashes += 1;
            } else {
                quoted.extend(std::iter::repeat_n(
                    '\\',
                    if character == '"' {
                        backslashes * 2 + 1
                    } else {
                        backslashes
                    },
                ));
                backslashes = 0;
                quoted.push(character);
            }
        }
        quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
        quoted.push('"');
        quoted
    }

    fn manage_autostart(action: Action, options: &ServiceOptions) -> Result<()> {
        let current_user = RegKey::predef(HKEY_CURRENT_USER);
        match action {
            Action::Install => {
                let (key, _) = current_user.create_subkey(RUN_KEY)?;
                let mut args = vec![options.executable.as_os_str().to_owned()];
                args.extend(arguments(options, "hidden-start"));
                let command = args
                    .iter()
                    .map(|arg| {
                        quote_argument(arg.to_str().expect("validated Unicode service arguments"))
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                // The Windows Run registry key has a documented 260-character limit.
                if command.encode_utf16().count() > 260 {
                    bail!(
                        "Windows login startup command exceeds 260 characters; use shorter executable/config paths or --scope system"
                    );
                }
                stop_autostart(options)?;
                key.set_value(SERVICE_NAME, &command)?;
                start_autostart(options)?;
                println!("Installed current-user login startup entry {SERVICE_NAME}");
            }
            Action::Uninstall => {
                match current_user.open_subkey_with_flags(RUN_KEY, KEY_WRITE) {
                    Ok(key) => match key.delete_value(SERVICE_NAME) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error.into()),
                    },
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
                stop_autostart(options)?;
                println!("Removed current-user login startup entry {SERVICE_NAME}");
            }
            Action::Start => start_autostart(options)?,
            Action::Stop => stop_autostart(options)?,
            Action::Status => {
                let value: String = current_user
                    .open_subkey_with_flags(RUN_KEY, KEY_READ)?
                    .get_value(SERVICE_NAME)?;
                println!("Login startup: {value}");
                println!("Bridge running: {}", autostart_running(options)?);
            }
        }
        Ok(())
    }

    fn start_autostart(options: &ServiceOptions) -> Result<()> {
        use std::os::windows::process::CommandExt;
        use std::process::{Command, Stdio};
        if autostart_running(options)? {
            println!("Bridge is already running");
            return Ok(());
        }
        std::fs::create_dir_all(&options.state_dir)?;
        let log_dir = options.state_dir.join("logs");
        std::fs::create_dir_all(&log_dir)?;
        let log_path = log_dir.join("bridge.log");
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        let mut child = Command::new(&options.executable)
            .args(arguments(options, "run"))
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .creation_flags(0x00000008 | 0x00000200 | 0x08000000)
            .spawn()
            .context("launch background bridge")?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait()? {
                bail!(
                    "background bridge exited during startup ({status}); inspect {}",
                    log_path.display()
                );
            }
            if autostart_running(options)?
                && runtime_pid(options).ok() == Some(u64::from(child.id()))
            {
                break;
            }
            if Instant::now() >= deadline {
                bail!(
                    "bridge did not acquire its runtime lock within 10 seconds; inspect {}",
                    log_path.display()
                );
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        println!("Started background bridge (PID {})", child.id());
        Ok(())
    }

    fn autostart_running(options: &ServiceOptions) -> Result<bool> {
        runtime_running(options)
    }

    fn runtime_pid(options: &ServiceOptions) -> Result<u64> {
        let state: serde_json::Value =
            serde_json::from_slice(&std::fs::read(options.state_dir.join("status.json"))?)?;
        state
            .get("pid")
            .and_then(serde_json::Value::as_u64)
            .context("runtime status has no PID")
    }

    fn stop_autostart(options: &ServiceOptions) -> Result<()> {
        // The foreground runtime owns this file protocol, so shutdown can finish
        // WebSocket and environment cleanup rather than terminating the process.
        if !autostart_running(options)? {
            return Ok(());
        }
        let pid = runtime_pid(options)?;
        crate::write_atomic(
            &options.state_dir.join("stop.request"),
            pid.to_string().as_bytes(),
        )?;
        let deadline = Instant::now() + Duration::from_secs(30);
        while autostart_running(options)? {
            if runtime_pid(options)? != pid {
                // The targeted process exited and a new instance acquired the
                // lock. Never stop a different generation with a stale request.
                return Ok(());
            }
            if Instant::now() >= deadline {
                bail!(
                    "bridge did not stop within 30 seconds; inspect {}",
                    options.state_dir.join("logs/bridge.log").display()
                );
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        Ok(())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod platform {
    use super::*;
    pub fn manage(_: Action, _: &ServiceOptions) -> Result<()> {
        bail!("service installation is supported on Windows, macOS, and Linux");
    }
}
