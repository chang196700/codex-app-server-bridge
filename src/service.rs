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

pub fn installed_executable(user_home: &Path) -> PathBuf {
    user_home
        .join(".codex-app-server-bridge")
        .join(if cfg!(windows) {
            "codex-app-server-bridge.exe"
        } else {
            "codex-app-server-bridge"
        })
}

pub fn manage(action: Action, options: &ServiceOptions) -> Result<()> {
    validate_paths(options)?;
    if action == Action::Install {
        if !std::env::current_exe()?.is_file() || !options.config_path.is_file() {
            bail!("source executable and configuration snapshot must exist before installation");
        }
        if options.executable != installed_executable(&options.user_home) {
            bail!(
                "services must use the installed executable in the target user's configuration directory"
            );
        }
        std::fs::create_dir_all(&options.state_dir).context("create service state directory")?;
    }
    platform::manage(action, options)
}

fn validate_paths(options: &ServiceOptions) -> Result<()> {
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
    Ok(())
}

#[derive(Clone, Copy, Debug)]
pub struct InstalledService {
    pub running: bool,
}

/// Inspect OS registration, not just the executable retained after uninstall.
/// An existing registration for a different executable/snapshot is an error.
pub fn inspect_installed(options: &ServiceOptions) -> Result<Option<InstalledService>> {
    validate_paths(options)?;
    if options.executable != installed_executable(&options.user_home) {
        bail!("service updates must target the fixed installed executable");
    }
    platform::inspect_installed(options)
}

/// Install an already downloaded and verified executable without changing the
/// registered service definition or its configuration snapshot.
pub fn update_binary(
    options: &ServiceOptions,
    candidate: &Path,
    expected_version: &semver::Version,
) -> Result<()> {
    inspect_installed(options)?.context("service is not installed; run `service install` first")?;
    let mut executable = ExecutableInstall::stage_from(options, candidate)?;
    crate::update::ensure_version(&options.executable, expected_version)?;
    // Recheck while holding the same lock used by service installation, after
    // download/verification and staging have completed.
    let previous =
        inspect_installed(options)?.context("service is no longer installed; update cancelled")?;
    let result = (|| -> Result<()> {
        if previous.running {
            platform::manage(Action::Stop, options)?;
        }
        executable.apply()?;
        if previous.running {
            let old_pid = status_pid(options);
            platform::manage(Action::Start, options)?;
            wait_for_runtime(options, old_pid)?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        let recovery = (|| -> Result<()> {
            if previous.running {
                platform::manage(Action::Stop, options)?;
            }
            executable.rollback()?;
            if previous.running {
                let old_pid = status_pid(options);
                platform::manage(Action::Start, options)?;
                wait_for_runtime(options, old_pid)?;
            }
            Ok(())
        })();
        return Err(match recovery {
            Ok(()) => error
                .context("service update failed; previous executable and running state restored"),
            Err(recovery) => error.context(format!(
                "service update failed and recovery was incomplete: {recovery:#}"
            )),
        });
    }
    executable.commit();
    Ok(())
}

fn status_pid(options: &ServiceOptions) -> Option<u64> {
    let bytes = std::fs::read(options.state_dir.join("status.json")).ok()?;
    serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()?
        .get("pid")?
        .as_u64()
}

fn wait_for_runtime(options: &ServiceOptions, old_pid: Option<u64>) -> Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        // The runtime writes its PID only after binding its TCP listener. The
        // instance lock alone is acquired earlier and does not confirm startup.
        if runtime_running(options)? && status_pid(options).is_some_and(|pid| Some(pid) != old_pid)
        {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            bail!(
                "updated service did not finish startup within 15 seconds; inspect {}",
                options.state_dir.display()
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn same_path(actual: &Path, expected: &Path) -> Result<bool> {
    let actual = std::fs::canonicalize(actual).unwrap_or_else(|_| actual.to_path_buf());
    let expected = std::fs::canonicalize(expected).unwrap_or_else(|_| expected.to_path_buf());
    #[cfg(windows)]
    return Ok(path_text(&actual)?.eq_ignore_ascii_case(path_text(&expected)?));
    #[cfg(not(windows))]
    Ok(actual == expected)
}

/// Owns staged files until service registration succeeds. Only platform code
/// decides when the running service has stopped and replacement is safe.
struct ExecutableInstall {
    destination: PathBuf,
    staged: Option<PathBuf>,
    backup: Option<PathBuf>,
    applied: bool,
    #[cfg(windows)]
    parked_self: Option<PathBuf>,
    _lock: std::fs::File,
}

impl ExecutableInstall {
    fn stage(options: &ServiceOptions) -> Result<Self> {
        Self::stage_from(options, &std::env::current_exe()?)
    }

    fn stage_from(options: &ServiceOptions, source: &Path) -> Result<Self> {
        let directory = options
            .executable
            .parent()
            .context("installed executable has no directory")?;
        if !directory.is_dir()
            || std::fs::symlink_metadata(directory)?
                .file_type()
                .is_symlink()
        {
            bail!("installation directory must already exist and must not be a symbolic link");
        }
        let lock_path = directory.join("install.lock");
        if std::fs::symlink_metadata(&lock_path)
            .is_ok_and(|metadata| !metadata.is_file() || metadata.file_type().is_symlink())
        {
            bail!("installation lock must be a regular file");
        }
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;
        fs2::FileExt::try_lock_exclusive(&lock)
            .context("another service installation is in progress")?;
        #[cfg(unix)]
        if options.scope == Scope::System {
            checked(
                std::process::Command::new("chown")
                    .arg(user_id(system_username(options)?)?.to_string())
                    .arg(&lock_path),
            )?;
        }
        let source = std::fs::canonicalize(source).context("locate source executable")?;
        let mut install = Self {
            destination: options.executable.clone(),
            staged: None,
            backup: None,
            applied: false,
            #[cfg(windows)]
            parked_self: None,
            _lock: lock,
        };
        let old_exists = match std::fs::symlink_metadata(&options.executable) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => true,
            Ok(_) => bail!("installed executable must be a regular file, not a symbolic link"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(error.into()),
        };
        if old_exists && source == std::fs::canonicalize(&options.executable)? {
            // Reinstalling from the fixed copy must not replace this process's
            // own mapped image on Windows. Its bytes already are the source.
            return Ok(install);
        }
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let prefix = format!(".bridge-install-{}-{nonce}", std::process::id());
        let staged = directory.join(format!("{prefix}.new"));
        copy_executable(&source, &staged, options)?;
        install.staged = Some(staged);
        if old_exists {
            let backup = directory.join(format!("{prefix}.backup"));
            copy_executable(&options.executable, &backup, options)?;
            install.backup = Some(backup);
        }
        Ok(install)
    }

    fn apply(&mut self) -> Result<()> {
        if let Some(staged) = &self.staged {
            #[cfg(windows)]
            if same_path(&std::env::current_exe()?, &self.destination)? {
                // Windows permits renaming a mapped image but not overwriting
                // it. Keep the updater's own image alive under a unique sibling
                // name until this process exits, with rollback still available.
                let parked = staged.with_extension("running.exe");
                std::fs::rename(&self.destination, &parked).context(
                    "move the running updater aside before replacing the service executable",
                )?;
                self.parked_self = Some(parked);
                self.applied = true;
            }
            replace_executable(staged, &self.destination).context(
                "replace installed executable; the old file was retained if replacement failed",
            )?;
            self.staged = None;
            self.applied = true;
        }
        Ok(())
    }

    fn rollback(&mut self) -> Result<()> {
        if self.applied {
            #[cfg(windows)]
            if let Some(parked) = &self.parked_self {
                replace_executable(parked, &self.destination).with_context(|| {
                    format!("restore running updater from {}", parked.display())
                })?;
                self.parked_self = None;
                self.applied = false;
                return Ok(());
            }
            if let Some(backup) = &self.backup {
                replace_executable(backup, &self.destination).with_context(|| {
                    format!(
                        "restore previous executable; recovery copy remains at {}",
                        backup.display()
                    )
                })?;
                self.backup = None;
            } else {
                std::fs::remove_file(&self.destination)?;
            }
            self.applied = false;
        }
        Ok(())
    }

    fn commit(mut self) {
        self.applied = false;
        #[cfg(windows)]
        if let Some(parked) = self.parked_self.take()
            && let Err(error) = self_replace::self_delete_at(&parked)
        {
            tracing::warn!(path = %parked.display(), %error, "update succeeded but the old updater image could not be scheduled for cleanup");
        }
        if let Some(backup) = self.backup.take()
            && let Err(error) = std::fs::remove_file(&backup)
        {
            tracing::warn!(path = %backup.display(), %error, "installation succeeded but old executable backup could not be removed");
        }
    }
}

impl Drop for ExecutableInstall {
    fn drop(&mut self) {
        if let Some(staged) = &self.staged {
            let _ = std::fs::remove_file(staged);
        }
        if !self.applied
            && let Some(backup) = &self.backup
        {
            let _ = std::fs::remove_file(backup);
        }
    }
}

fn copy_executable(source: &Path, destination: &Path, options: &ServiceOptions) -> Result<()> {
    let mut input = std::fs::File::open(source)?;
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let result = (|| -> Result<()> {
        let expected_length = input.metadata()?.len();
        let written = std::io::copy(&mut input, &mut output)?;
        if written != expected_length || written == 0 {
            bail!("source executable changed while being copied");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            output.set_permissions(std::fs::Permissions::from_mode(0o755))?;
            if options.scope == Scope::System {
                checked(
                    std::process::Command::new("chown")
                        .arg(user_id(system_username(options)?)?.to_string())
                        .arg(destination),
                )?;
            }
        }
        #[cfg(not(unix))]
        let _ = options;
        output.sync_all()?;
        Ok(())
    })();
    drop(output);
    if result.is_err() {
        let _ = std::fs::remove_file(destination);
    }
    result.with_context(|| format!("copy executable to {}", destination.display()))
}

fn replace_executable(source: &Path, destination: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match std::fs::rename(source, destination) {
                Ok(()) => return Ok(()),
                Err(error)
                    if error.kind() == std::io::ErrorKind::PermissionDenied
                        && std::time::Instant::now() < deadline =>
                {
                    // SCM can report Stopped shortly before the process releases
                    // its executable image. Never delete the old file first.
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(source, destination)?;
        Ok(())
    }
}

fn installation_failure(error: anyhow::Error, recovery: Result<()>) -> anyhow::Error {
    match recovery {
        Ok(()) => error.context("installation failed; previous executable/service registration restored where present (the configuration snapshot is not rolled back)"),
        Err(recovery) => error.context(format!("installation failed and recovery was incomplete: {recovery:#}")),
    }
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
    if std::fs::symlink_metadata(path)
        .is_ok_and(|metadata| !metadata.is_file() || metadata.file_type().is_symlink())
    {
        bail!(
            "service definition must be a regular file: {}",
            path.display()
        );
    }
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
fn previous_definition(path: &Path) -> Result<Option<String>> {
    check_managed(path)?;
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(Some(content)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[cfg(unix)]
fn restore_definition(path: &Path, previous: Option<&str>) -> Result<()> {
    if let Some(previous) = previous {
        write_managed(path, previous)
    } else {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
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

    pub fn inspect_installed(options: &ServiceOptions) -> Result<Option<InstalledService>> {
        let path = unit_path(options);
        let Some(content) = previous_definition(&path)? else {
            // No installation for this scope/home. In particular, do not need
            // a running user manager just to report that nothing is installed.
            return Ok(None);
        };
        let name = format!("{SERVICE_NAME}.service");
        let output = systemctl(options.scope)
            .args([
                "show",
                &name,
                "--property=FragmentPath",
                "--property=LoadState",
                "--property=ActiveState",
                "--property=DropInPaths",
            ])
            .output()
            .context("inspect installed systemd service")?;
        let properties = String::from_utf8(output.stdout)?;
        let property = |key: &str| properties.lines().find_map(|line| line.strip_prefix(key));
        if !output.status.success() {
            bail!(
                "cannot inspect installed systemd service: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        if property("LoadState=") != Some("loaded") {
            bail!(
                "installed systemd service is not loadable; repair it with `service install` before updating"
            );
        }
        let fragment = property("FragmentPath=").context("systemd returned no FragmentPath")?;
        if !same_path(Path::new(fragment), &path)? {
            bail!("refusing to update a systemd service loaded from another file: {fragment}");
        }
        if property("DropInPaths=").is_some_and(|value| !value.is_empty()) {
            bail!(
                "service has systemd drop-in overrides; remove overrides before updating so its executable and configuration can be verified"
            );
        }
        let mut expected = vec![quote(path_text(&options.executable)?, true)];
        expected.extend(arguments(options, "run").iter().map(|arg| {
            quote(
                arg.to_str().expect("validated Unicode service arguments"),
                true,
            )
        }));
        let commands = content
            .lines()
            .filter_map(|line| line.trim().strip_prefix("ExecStart="))
            .collect::<Vec<_>>();
        if commands != [expected.join(" ")] {
            bail!("existing systemd service uses a different executable or configuration snapshot");
        }
        if options.scope == Scope::System {
            let expected_user = user_id(system_username(options)?)?.to_string();
            let users = content
                .lines()
                .filter_map(|line| line.trim().strip_prefix("User="))
                .collect::<Vec<_>>();
            if users != [expected_user] {
                bail!("existing systemd service runs as a different user");
            }
        }
        let running = match property("ActiveState=") {
            Some("active" | "activating" | "reloading") => true,
            Some("inactive" | "failed") => false,
            state => bail!("systemd service is changing state ({state:?}); retry after it settles"),
        };
        Ok(Some(InstalledService { running }))
    }

    fn stop_if_running(scope: Scope, name: &str) -> Result<()> {
        let output =
            checked(systemctl(scope).args(["show", name, "--property=ActiveState", "--value"]))?;
        let state = String::from_utf8(output.stdout)?;
        let state = state.trim();
        if state.is_empty() {
            bail!("systemd returned no ActiveState for {name}");
        }
        // A malformed managed unit can be inactive with LoadState=bad-setting.
        // Asking systemd to stop it then fails with "not loaded" and would
        // prevent repairing or removing its definition. Only an explicitly
        // inactive unit may skip stop; inspection and all other errors propagate.
        if state != "inactive" {
            checked(systemctl(scope).args(["stop", name]))?;
        }
        Ok(())
    }

    pub fn manage(action: Action, options: &ServiceOptions) -> Result<()> {
        let path = unit_path(options);
        let name = format!("{SERVICE_NAME}.service");
        match action {
            Action::Install => {
                let previous = previous_definition(&path)?;
                let inspection = systemctl(options.scope)
                    .args([
                        "show",
                        &name,
                        "--property=FragmentPath",
                        "--property=LoadState",
                        "--property=ActiveState",
                        "--property=UnitFileState",
                    ])
                    .output()
                    .context("inspect existing systemd service")?;
                let properties = String::from_utf8(inspection.stdout)?;
                let property =
                    |key: &str| properties.lines().find_map(|line| line.strip_prefix(key));
                if !inspection.status.success() && property("LoadState=") != Some("not-found") {
                    bail!(
                        "cannot inspect existing systemd service: {}",
                        String::from_utf8_lossy(&inspection.stderr)
                    );
                }
                if let Some(fragment) = property("FragmentPath=").filter(|value| !value.is_empty())
                    && (previous.is_none()
                        || std::fs::canonicalize(fragment)? != std::fs::canonicalize(&path)?)
                {
                    bail!(
                        "refusing to replace an unmanaged systemd service loaded from {fragment}"
                    );
                }
                let was_running = matches!(
                    property("ActiveState="),
                    Some("active" | "activating" | "reloading")
                );
                let was_enabled = matches!(
                    property("UnitFileState="),
                    Some("enabled" | "enabled-runtime")
                );
                let identity = if options.scope == Scope::System {
                    let username = system_username(options)?;
                    prepare_system_files(options, username)?;
                    // User= is a scalar, unlike ExecStart/Environment. A numeric
                    // UID avoids quoting or specifier interpretation entirely.
                    format!("User={}\n", user_id(username)?)
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
                // Codex detaches its daemon with setsid but stays in the unit's
                // cgroup. Stop only the bridge so service updates/restarts do
                // not terminate the independently managed Codex daemon.
                let content = format!(
                    "# {MANAGED_MARKER}\n[Unit]\nDescription=Codex app-server WebSocket bridge\nAfter=network.target\n\n[Service]\nType=exec\n{identity}ExecStart={}\nEnvironment={}\nEnvironment={}\nRestart=on-failure\nRestartSec=3\nKillMode=process\nTimeoutStopSec=30\nUMask=0077\n\n[Install]\nWantedBy={target}\n",
                    command.join(" "),
                    quote(&format!("HOME={}", path_text(&options.user_home)?), false),
                    quote(
                        &format!("CODEX_HOME={}", path_text(&options.codex_home)?),
                        false
                    ),
                );
                let mut executable = ExecutableInstall::stage(options)?;
                let mut registration_changed = false;
                let result = (|| -> Result<()> {
                    if previous.is_some() {
                        stop_if_running(options.scope, &name)?;
                    }
                    executable.apply()?;
                    write_managed(&path, &content)?;
                    registration_changed = true;
                    checked(systemctl(options.scope).arg("daemon-reload"))?;
                    checked(systemctl(options.scope).args(["enable", &name]))?;
                    checked(systemctl(options.scope).args(["start", &name]))?;
                    Ok(())
                })();
                if let Err(error) = result {
                    let recovery = (|| -> Result<()> {
                        if registration_changed {
                            stop_if_running(options.scope, &name)?;
                            if !was_enabled {
                                checked(systemctl(options.scope).args(["disable", &name]))?;
                            }
                        }
                        executable.rollback()?;
                        restore_definition(&path, previous.as_deref())?;
                        checked(systemctl(options.scope).arg("daemon-reload"))?;
                        if was_running {
                            checked(systemctl(options.scope).args(["start", &name]))?;
                        }
                        Ok(())
                    })();
                    return Err(installation_failure(error, recovery));
                }
                executable.commit();
                println!(
                    "Installed {} and started {}",
                    options.executable.display(),
                    path.display()
                );
                if options.scope == Scope::User {
                    println!(
                        "For startup before login, enable lingering for this user with loginctl enable-linger."
                    );
                }
            }
            Action::Uninstall => {
                check_managed(&path)?;
                if path.exists() {
                    stop_if_running(options.scope, &name)?;
                    checked(systemctl(options.scope).args(["disable", &name]))?;
                    std::fs::remove_file(&path)?;
                    checked(systemctl(options.scope).arg("daemon-reload"))?;
                }
                println!("Uninstalled {name}");
            }
            Action::Stop => stop_if_running(options.scope, &name)?,
            Action::Start | Action::Status => {
                let verb = match action {
                    Action::Start => "start",
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

    pub fn inspect_installed(options: &ServiceOptions) -> Result<Option<InstalledService>> {
        let directory = match options.scope {
            Scope::User => options.user_home.join("Library/LaunchAgents"),
            Scope::System => PathBuf::from("/Library/LaunchDaemons"),
        };
        let path = directory.join(format!("{LABEL}.plist"));
        if previous_definition(&path)?.is_none() {
            // A user without a GUI launchd domain can still ask whether the
            // service is installed. An uninstall deliberately retains binaries.
            return Ok(None);
        }
        let target = format!("{}/{LABEL}", domain(options.scope)?);
        let is_loaded = loaded(&target)?;
        let extract = |key: &str, format: &str| -> Result<Vec<u8>> {
            Ok(checked(
                Command::new("plutil")
                    .args(["-extract", key, format, "-o", "-"])
                    .arg(&path),
            )?
            .stdout)
        };
        let label = String::from_utf8(extract("Label", "raw")?)?;
        if label.trim() != LABEL {
            bail!("existing launchd service has a different label");
        }
        let actual: Vec<String> = serde_json::from_slice(&extract("ProgramArguments", "json")?)?;
        let mut expected = vec![path_text(&options.executable)?.to_owned()];
        expected.extend(arguments(options, "run").iter().map(|arg| {
            arg.to_str()
                .expect("validated Unicode service arguments")
                .to_owned()
        }));
        if actual != expected {
            bail!("existing launchd service uses a different executable or configuration snapshot");
        }
        if options.scope == Scope::System {
            let username = String::from_utf8(extract("UserName", "raw")?)?;
            if username.trim() != system_username(options)? {
                bail!("existing launchd service runs as a different user");
            }
        }
        if is_loaded {
            let output = checked(Command::new("launchctl").args(["print", &target]))?;
            let description = String::from_utf8(output.stdout)?;
            let field = |key: &str| {
                description
                    .lines()
                    .find_map(|line| line.trim().strip_prefix(key))
            };
            let registered_path =
                field("path = ").context("launchd returned no registered plist path")?;
            let program = field("program = ").context("launchd returned no registered program")?;
            if !same_path(Path::new(registered_path), &path)?
                || !same_path(Path::new(program), &options.executable)?
            {
                bail!("loaded launchd service does not match its managed installation");
            }
            let args = description
                .lines()
                .skip_while(|line| line.trim() != "arguments = {")
                .skip(1)
                .take_while(|line| line.trim() != "}")
                .map(str::trim)
                .collect::<Vec<_>>();
            if args != expected {
                bail!(
                    "loaded launchd service uses different arguments; reinstall it before updating"
                );
            }
        }
        // A loaded KeepAlive job may be waiting for a restart. Boot it out before
        // replacement and restore it afterwards, just like a running job.
        Ok(Some(InstalledService { running: is_loaded }))
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
                let previous = previous_definition(&path)?;
                let was_loaded = loaded(&target)?;
                if was_loaded && previous.is_none() {
                    bail!("refusing to replace an unmanaged launchd job {target}");
                }
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
                let mut executable = ExecutableInstall::stage(options)?;
                let result = (|| -> Result<()> {
                    if was_loaded {
                        bootout(&target, options)?;
                    }
                    executable.apply()?;
                    write_managed(&path, &content)?;
                    checked(Command::new("launchctl").args(["enable", &target]))?;
                    checked(
                        Command::new("launchctl")
                            .arg("bootstrap")
                            .arg(&domain)
                            .arg(&path),
                    )?;
                    Ok(())
                })();
                if let Err(error) = result {
                    let recovery = (|| -> Result<()> {
                        if loaded(&target)? {
                            bootout(&target, options)?;
                        }
                        executable.rollback()?;
                        restore_definition(&path, previous.as_deref())?;
                        if was_loaded {
                            checked(
                                Command::new("launchctl")
                                    .arg("bootstrap")
                                    .arg(&domain)
                                    .arg(&path),
                            )?;
                        }
                        Ok(())
                    })();
                    return Err(installation_failure(error, recovery));
                }
                executable.commit();
                println!(
                    "Installed {} and started {}",
                    options.executable.display(),
                    path.display()
                );
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

    pub fn inspect_installed(options: &ServiceOptions) -> Result<Option<InstalledService>> {
        if options.username.is_some() {
            bail!("--username is only supported for Linux and macOS system services");
        }
        let (command, running) = match options.scope {
            Scope::User => {
                let Some(command) = current_autostart(&RegKey::predef(HKEY_CURRENT_USER))? else {
                    return Ok(None);
                };
                (
                    managed_command(&command, options, "hidden-start")?,
                    autostart_running(options)?,
                )
            }
            Scope::System => {
                let manager =
                    ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
                        .context("open Windows Service Control Manager")?;
                let service = match manager.open_service(
                    SERVICE_NAME,
                    ServiceAccess::QUERY_CONFIG | ServiceAccess::QUERY_STATUS,
                ) {
                    Ok(service) => service,
                    Err(error) if missing_service(&error) => return Ok(None),
                    Err(error) => return Err(error.into()),
                };
                let config = service.query_config()?;
                if config.display_name != OsStr::new("Codex app-server WebSocket bridge")
                    || config.service_type != ServiceType::OWN_PROCESS
                {
                    bail!("refusing to update an unmanaged Windows service {SERVICE_NAME}");
                }
                let state = service.query_status()?.current_state;
                let running = match state {
                    ServiceState::Running | ServiceState::StartPending => true,
                    ServiceState::Stopped => false,
                    _ => bail!(
                        "Windows service is in state {state:?}; stop or start it before updating"
                    ),
                };
                (
                    managed_command(path_text(&config.executable_path)?, options, "service-run")?,
                    running,
                )
            }
        };
        if !same_path(Path::new(&command[0]), &options.executable)? {
            bail!("existing Windows service uses a different executable");
        }
        Ok(Some(InstalledService { running }))
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
                | ServiceAccess::QUERY_CONFIG
                | ServiceAccess::CHANGE_CONFIG
                | ServiceAccess::START
                | ServiceAccess::STOP
                | ServiceAccess::DELETE;
            let mut service = match manager.open_service(SERVICE_NAME, access) {
                Ok(service) => Some(service),
                Err(error) if missing_service(&error) => None,
                Err(error) => return Err(error.into()),
            };
            let previous = if let Some(service) = &service {
                let config = service.query_config()?;
                if config.display_name != OsStr::new("Codex app-server WebSocket bridge")
                    || config.service_type != ServiceType::OWN_PROCESS
                {
                    bail!("refusing to modify an unmanaged Windows service {SERVICE_NAME}");
                }
                let command =
                    managed_command(path_text(&config.executable_path)?, options, "service-run")?;
                Some(ServiceInfo {
                    name: SERVICE_NAME.into(),
                    display_name: config.display_name,
                    service_type: config.service_type,
                    start_type: config.start_type,
                    error_control: config.error_control,
                    executable_path: PathBuf::from(&command[0]),
                    launch_arguments: command[1..].to_vec(),
                    dependencies: config.dependencies,
                    account_name: config.account_name,
                    account_password: None,
                })
            } else {
                None
            };
            let was_running = if let Some(service) = &service {
                service.query_status()?.current_state != ServiceState::Stopped
            } else {
                false
            };
            let mut executable = ExecutableInstall::stage(options)?;
            let result = (|| -> Result<()> {
                if let Some(service) = &service {
                    stop_service(service)?;
                }
                executable.apply()?;
                if let Some(service) = &service {
                    service.change_config(&info)?;
                } else {
                    service = Some(manager.create_service(&info, access)?);
                }
                let service = service.as_ref().context("missing installed service")?;
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
                wait_for(service, ServiceState::Running)?;
                Ok(())
            })();
            if let Err(error) = result {
                let recovery = (|| -> Result<()> {
                    if let Some(service) = &service {
                        stop_service(service)?;
                    }
                    executable.rollback()?;
                    if let Some(service) = &service {
                        if let Some(previous) = &previous {
                            service.change_config(previous)?;
                            if was_running {
                                service.start::<&OsStr>(&[])?;
                                wait_for(service, ServiceState::Running)?;
                            }
                        } else {
                            service.delete()?;
                        }
                    }
                    Ok(())
                })();
                return Err(installation_failure(error, recovery));
            }
            executable.commit();
            println!(
                "Installed {} and started Windows service {SERVICE_NAME}",
                options.executable.display()
            );
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

    fn managed_command(
        command: &str,
        options: &ServiceOptions,
        subcommand: &str,
    ) -> Result<Vec<OsString>> {
        let characters: Vec<char> = command.chars().collect();
        let mut index = 0;
        let mut arguments = Vec::new();
        while index < characters.len() {
            while index < characters.len() && characters[index].is_whitespace() {
                index += 1;
            }
            if index == characters.len() {
                break;
            }
            let mut argument = String::new();
            let mut quoted = false;
            while index < characters.len() && (quoted || !characters[index].is_whitespace()) {
                let mut slashes = 0;
                while index < characters.len() && characters[index] == '\\' {
                    slashes += 1;
                    index += 1;
                }
                if index < characters.len() && characters[index] == '"' {
                    argument.extend(std::iter::repeat_n('\\', slashes / 2));
                    if slashes % 2 == 0 {
                        quoted = !quoted;
                    } else {
                        argument.push('"');
                    }
                    index += 1;
                } else {
                    argument.extend(std::iter::repeat_n('\\', slashes));
                    if index < characters.len() && (quoted || !characters[index].is_whitespace()) {
                        argument.push(characters[index]);
                        index += 1;
                    }
                }
            }
            if quoted {
                bail!("refusing to modify a service with an unrecognized command line");
            }
            arguments.push(argument);
        }
        if arguments.len() != 4
            || !Path::new(&arguments[0]).is_absolute()
            || arguments[1] != "--config"
            || arguments[3] != subcommand
        {
            bail!("refusing to modify an unmanaged service command: {command}");
        }
        let previous_config =
            std::fs::canonicalize(&arguments[2]).unwrap_or_else(|_| PathBuf::from(&arguments[2]));
        let expected_config = std::fs::canonicalize(&options.config_path)?;
        if !previous_config
            .to_string_lossy()
            .eq_ignore_ascii_case(&expected_config.to_string_lossy())
        {
            bail!(
                "existing service uses a different configuration snapshot: {}",
                arguments[2]
            );
        }
        Ok(arguments.into_iter().map(OsString::from).collect())
    }

    fn current_autostart(current_user: &RegKey) -> Result<Option<String>> {
        match current_user.open_subkey_with_flags(RUN_KEY, KEY_READ) {
            Ok(key) => match key.get_value(SERVICE_NAME) {
                Ok(value) => Ok(Some(value)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error.into()),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn manage_autostart(action: Action, options: &ServiceOptions) -> Result<()> {
        let current_user = RegKey::predef(HKEY_CURRENT_USER);
        match action {
            Action::Install => {
                let previous = current_autostart(&current_user)?;
                let previous_arguments = previous
                    .as_deref()
                    .map(|command| managed_command(command, options, "hidden-start"))
                    .transpose()?;
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
                        "Windows login startup command exceeds 260 characters; use a shorter target home/config path or --scope system"
                    );
                }
                let was_running = autostart_running(options)?;
                if was_running && previous.is_none() {
                    bail!("stop the foreground bridge before installing a new login startup entry");
                }
                let mut executable = ExecutableInstall::stage(options)?;
                let (key, _) = current_user.create_subkey(RUN_KEY)?;
                let result = (|| -> Result<()> {
                    stop_autostart(options)?;
                    executable.apply()?;
                    key.set_value(SERVICE_NAME, &command)?;
                    start_autostart(options)?;
                    Ok(())
                })();
                if let Err(error) = result {
                    let recovery = (|| -> Result<()> {
                        stop_autostart(options)?;
                        executable.rollback()?;
                        if let Some(previous) = &previous {
                            key.set_value(SERVICE_NAME, previous)?;
                        } else {
                            match key.delete_value(SERVICE_NAME) {
                                Ok(()) => {}
                                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                                Err(error) => return Err(error.into()),
                            }
                        }
                        if was_running {
                            let mut old_options = options.clone();
                            let previous_arguments = previous_arguments
                                .as_ref()
                                .context("missing previous startup arguments")?;
                            old_options.executable = PathBuf::from(&previous_arguments[0]);
                            old_options.config_path = PathBuf::from(&previous_arguments[2]);
                            start_autostart(&old_options)?;
                        }
                        Ok(())
                    })();
                    return Err(installation_failure(error, recovery));
                }
                executable.commit();
                println!(
                    "Installed {} and current-user login startup entry {SERVICE_NAME}",
                    options.executable.display()
                );
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
    pub fn inspect_installed(_: &ServiceOptions) -> Result<Option<InstalledService>> {
        bail!("service updates are supported on Windows, macOS, and Linux");
    }
    pub fn manage(_: Action, _: &ServiceOptions) -> Result<()> {
        bail!("service installation is supported on Windows, macOS, and Linux");
    }
}
