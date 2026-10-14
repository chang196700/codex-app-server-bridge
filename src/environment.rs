//! Publish the bridge URL for future processes belonging to the configured user.
//!
//! An operating system cannot change the inherited environment of an already
//! running terminal or application. The shell file can also be sourced manually.
//! A Windows service's Session 0 broadcast cannot reach another session's
//! Explorer; the target user's registry value is still persisted for later login.

use anyhow::{Context, Result, bail};
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const VARIABLE: &str = "CODEX_APP_SERVER_WS_URL";
const FILE_HEADER: &str = "# Managed by codex-app-server-bridge.\n";
#[cfg(unix)]
const HOOK_START: &str = "# >>> codex-app-server-bridge >>>";
#[cfg(unix)]
const HOOK_END: &str = "# <<< codex-app-server-bridge <<<";

pub struct Publisher {
    #[cfg(unix)]
    user_home: PathBuf,
    state_dir: PathBuf,
    #[cfg(windows)]
    windows_user_sid: Option<String>,
    #[cfg(unix)]
    session_pending: bool,
    url: Option<String>,
}

impl Publisher {
    /// Construct only after obtaining the bridge's instance lock. An owned
    /// state record recovers the previous URL after an unclean shutdown.
    pub fn new(user_home: PathBuf, state_dir: PathBuf, windows_user_sid: Option<String>) -> Self {
        let url = fs::read_to_string(state_dir.join("published-url"))
            .ok()
            .and_then(|text| text.strip_prefix(FILE_HEADER).map(str::to_owned))
            .map(|text| text.trim_end_matches('\n').to_owned())
            .filter(|url| validate_url(url).is_ok());
        #[cfg(not(unix))]
        let _ = user_home;
        #[cfg(not(windows))]
        let _ = windows_user_sid;
        Self {
            #[cfg(unix)]
            user_home,
            state_dir,
            #[cfg(windows)]
            windows_user_sid,
            #[cfg(unix)]
            session_pending: false,
            url,
        }
    }

    /// Called only after both the socket and WebSocket listener are ready.
    pub fn publish(&mut self, url: &str) -> Result<()> {
        validate_url(url)?;
        if !self.state_dir.is_absolute() {
            bail!("environment state directory must be absolute");
        }
        #[cfg(unix)]
        if !self.user_home.is_absolute() {
            bail!("environment user home must be absolute");
        }
        if self.url.as_deref().is_some_and(|old| old != url) {
            self.clear()?;
        }
        // Write the ownership record first: recovery compares the actual
        // destination value before removing anything, even after a crash.
        atomic_write(
            &self.state_dir.join("published-url"),
            format!("{FILE_HEADER}{url}\n").as_bytes(),
        )?;
        self.url = Some(url.to_owned());
        #[cfg(windows)]
        self.publish_windows(url)?;
        #[cfg(unix)]
        self.publish_unix(url)?;
        tracing::info!(
            url,
            variable = VARIABLE,
            "published user environment variable"
        );
        Ok(())
    }

    /// Remove only a value (or file) which still matches this bridge's URL.
    /// User changes and other applications' values are preserved.
    pub fn clear(&mut self) -> Result<()> {
        let Some(url) = self.url.as_deref() else {
            return Ok(());
        };
        #[cfg(windows)]
        self.clear_windows(url)?;
        #[cfg(unix)]
        self.clear_unix(url)?;
        remove_if_equal(
            &self.state_dir.join("published-url"),
            &format!("{FILE_HEADER}{url}\n"),
        )?;
        self.url = None;
        #[cfg(unix)]
        {
            self.session_pending = false;
        }
        Ok(())
    }

    /// Retry a missing session manager after user login, without rewriting
    /// profile files or repeatedly overwriting a successfully published value.
    /// Call from the same blocking worker as publish/clear: each OS command has
    /// a two-second timeout, and the macOS domain lookup uses several commands.
    pub fn refresh_session(&mut self) {
        #[cfg(unix)]
        if self.session_pending
            && let Some(url) = self.url.as_deref()
        {
            self.session_pending = !update_session_environment(Some(url));
        }
    }

    #[cfg(unix)]
    fn shell_path(&self) -> PathBuf {
        self.state_dir.join("environment.sh")
    }

    #[cfg(unix)]
    fn shell_contents(url: &str) -> String {
        format!("{FILE_HEADER}export {VARIABLE}={}\n", shell_quote(url))
    }

    #[cfg(target_os = "linux")]
    fn environment_d_path(&self) -> PathBuf {
        self.user_home
            .join(".config/environment.d/90-codex-app-server-bridge.conf")
    }

    #[cfg(target_os = "linux")]
    fn environment_d_contents(url: &str) -> String {
        // validate_url excludes expansion, quoting and comment metacharacters.
        format!("{FILE_HEADER}{VARIABLE}={url}\n")
    }

    #[cfg(unix)]
    fn shell_hook(&self) -> Result<String> {
        let path = self.shell_path();
        let path = path
            .to_str()
            .context("shell environment path is not valid UTF-8")?;
        if path.contains(['\r', '\n']) {
            bail!("shell environment path must not contain a newline");
        }
        let quoted_path = shell_quote(path);
        Ok(format!(
            "{HOOK_START}\nif [ -r {quoted_path} ]; then\n    . {quoted_path}\nfi\n{HOOK_END}\n"
        ))
    }

    #[cfg(unix)]
    fn shell_profiles(&self) -> Vec<PathBuf> {
        let mut paths = vec![
            self.user_home.join(".profile"),
            self.user_home.join(".zprofile"),
        ];
        // Creating .bash_profile when it did not exist would prevent Bash from
        // reading an existing .profile, so touch Bash's overrides only if present.
        for name in [".bash_profile", ".bash_login"] {
            let path = self.user_home.join(name);
            if path.exists() {
                paths.push(path);
            }
        }
        paths
    }

    #[cfg(unix)]
    fn publish_unix(&mut self, url: &str) -> Result<()> {
        let hook = self.shell_hook()?;
        atomic_write(&self.shell_path(), Self::shell_contents(url).as_bytes())?;
        #[cfg(target_os = "linux")]
        atomic_write(
            &self.environment_d_path(),
            Self::environment_d_contents(url).as_bytes(),
        )?;
        for path in self.shell_profiles() {
            if let Err(error) = install_hook(&path, &hook) {
                tracing::warn!(path = %path.display(), %error, "could not install shell environment hook; source environment.sh manually");
            }
        }
        self.session_pending = !update_session_environment(Some(url));
        Ok(())
    }

    #[cfg(unix)]
    fn clear_unix(&self, url: &str) -> Result<()> {
        // Clean each independent destination even if one file is unavailable.
        let mut first_error = None;
        let shell_owned = match remove_if_equal(&self.shell_path(), &Self::shell_contents(url)) {
            Ok(owned) => owned,
            Err(error) => {
                first_error = Some(error);
                false
            }
        };
        #[cfg(target_os = "linux")]
        if let Err(error) = remove_if_equal(
            &self.environment_d_path(),
            &Self::environment_d_contents(url),
        ) {
            first_error.get_or_insert(error);
        }
        if shell_owned {
            match self.shell_hook() {
                Ok(hook) => {
                    for path in self.shell_profiles() {
                        if let Err(error) = remove_hook(&path, &hook) {
                            tracing::warn!(path = %path.display(), %error, "could not remove shell environment hook");
                        }
                    }
                }
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        clear_session_environment_if_equal(url);
        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(())
    }

    #[cfg(windows)]
    fn registry_environment(&self) -> Result<winreg::RegKey> {
        use winreg::RegKey;
        use winreg::enums::{HKEY_CURRENT_USER, HKEY_USERS, KEY_READ, KEY_WRITE};
        let root = if let Some(sid) = &self.windows_user_sid {
            validate_sid(sid)?;
            // Never create an unloaded SID key: that would be an ordinary HKU
            // key, not the user's actual profile hive. Retry after user logon.
            RegKey::predef(HKEY_USERS)
                .open_subkey_with_flags(sid, KEY_READ | KEY_WRITE)
                .context("target user's Windows profile hive is not loaded; environment publication requires user logon")?
        } else {
            RegKey::predef(HKEY_CURRENT_USER)
        };
        root.create_subkey("Environment")
            .map(|(key, _)| key)
            .context("open the target user's Windows environment registry key")
    }

    #[cfg(windows)]
    fn publish_windows(&self, url: &str) -> Result<()> {
        self.registry_environment()?
            .set_value(VARIABLE, &url)
            .context("set the user's CODEX_APP_SERVER_WS_URL registry value")?;
        broadcast_environment_change();
        Ok(())
    }

    #[cfg(windows)]
    fn clear_windows(&self, url: &str) -> Result<()> {
        let key = self.registry_environment()?;
        match key.get_value::<String, _>(VARIABLE) {
            Ok(current) if current == url => {
                key.delete_value(VARIABLE)
                    .context("remove the bridge's Windows environment value")?;
                broadcast_environment_change();
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("read the user's Windows environment value"),
        }
        Ok(())
    }
}

fn validate_url(url: &str) -> Result<()> {
    // This variable contains the listener address, never arbitrary WS request
    // paths, credentials or query strings. A restricted alphabet also makes the
    // environment.d assignment literal (no shell/systemd interpolation).
    if !(url.starts_with("ws://") || url.starts_with("wss://"))
        || url.len() <= 5
        || !url
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b":/.[]_-%".contains(&byte))
    {
        bail!("invalid WebSocket listener URL for environment publication: {url:?}");
    }
    Ok(())
}

#[cfg(unix)]
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(unix)]
fn install_hook(path: &Path, hook: &str) -> Result<()> {
    refuse_symlink(path)?;
    let mut content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    if content.contains(hook) {
        return Ok(());
    }
    if content.contains(HOOK_START) || content.contains(HOOK_END) {
        bail!("existing bridge shell hook differs; refusing to overwrite it");
    }
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(hook);
    atomic_write(path, content.as_bytes())
}

#[cfg(unix)]
fn remove_hook(path: &Path, hook: &str) -> Result<()> {
    refuse_symlink(path)?;
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    if content.contains(hook) {
        atomic_write(path, content.replacen(hook, "", 1).as_bytes())?;
    }
    Ok(())
}

fn refuse_symlink(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("refusing to replace symlink {}", path.display());
        }
        Ok(metadata) if !metadata.is_file() => {
            bail!("expected a regular file at {}", path.display());
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("inspect {}", path.display())),
    }
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<()> {
    refuse_symlink(path)?;
    let parent = path
        .parent()
        .context("environment file has no parent directory")?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let name = path
        .file_name()
        .context("environment file has no filename")?;
    let temporary = parent.join(format!(
        ".{}.{}.{}.tmp",
        name.to_string_lossy(),
        std::process::id(),
        nonce
    ));
    let result = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        // Existing profile permissions are retained; new environment files are
        // readable only by their owner on Unix.
        if let Ok(metadata) = fs::metadata(path) {
            file.set_permissions(metadata.permissions())?;
        }
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        replace_file(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.with_context(|| format!("write {}", path.display()))
}

#[cfg(not(windows))]
fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::rename(from, to)
}

#[cfg(windows)]
fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: Both UTF-16 paths are NUL-terminated and remain alive for the call.
    let result = unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// True means this file was ours or already absent. Different contents are
/// deliberately preserved, including a file which no longer has our header.
fn remove_if_equal(path: &Path, expected: &str) -> Result<bool> {
    refuse_symlink(path)?;
    let actual = match fs::read(path) {
        Ok(actual) => actual,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    if actual != expected.as_bytes() {
        return Ok(false);
    }
    fs::remove_file(path).with_context(|| format!("remove {}", path.display()))?;
    Ok(true)
}

/// Drain output concurrently so a full pipe cannot deadlock the process. A
/// command or descendant retaining a pipe cannot block the caller indefinitely.
fn run_command(command: &mut Command) -> std::io::Result<Output> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    fn read_pipe(
        pipe: impl Read + Send + 'static,
    ) -> std::sync::mpsc::Receiver<std::io::Result<Vec<u8>>> {
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = pipe
                .take(1024 * 1024)
                .read_to_end(&mut bytes)
                .map(|_| bytes);
            let _ = send.send(result);
        });
        receive
    }
    let stdout = read_pipe(child.stdout.take().expect("stdout was piped"));
    let stderr = read_pipe(child.stderr.take().expect("stderr was piped"));
    let deadline = Instant::now() + Duration::from_secs(2);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::new(
                    ErrorKind::TimedOut,
                    format!(
                        "{:?} exceeded the two-second command timeout",
                        command.get_program()
                    ),
                ));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        }
    };
    fn receive_output(
        receive: std::sync::mpsc::Receiver<std::io::Result<Vec<u8>>>,
    ) -> std::io::Result<Vec<u8>> {
        receive
            .recv_timeout(Duration::from_millis(100))
            .map_err(|_| {
                std::io::Error::new(ErrorKind::TimedOut, "command output pipe did not close")
            })?
    }
    Ok(Output {
        status,
        stdout: receive_output(stdout)?,
        stderr: receive_output(stderr)?,
    })
}

#[cfg(target_os = "linux")]
fn session_command() -> std::process::Command {
    let mut command = std::process::Command::new("systemctl");
    command.arg("--user");
    // System services using User= may lack the login session's environment.
    // A manager may not exist until login; that case remains a nonfatal warning.
    if std::env::var_os("XDG_RUNTIME_DIR").is_none()
        && let Ok(output) = run_command(std::process::Command::new("id").arg("-u"))
    {
        let uid = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if output.status.success() && !uid.is_empty() && uid.bytes().all(|b| b.is_ascii_digit()) {
            let runtime = format!("/run/user/{uid}");
            if Path::new(&runtime).is_dir() {
                command.env("XDG_RUNTIME_DIR", &runtime);
                if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
                    command.env(
                        "DBUS_SESSION_BUS_ADDRESS",
                        format!("unix:path={runtime}/bus"),
                    );
                }
            }
        }
    }
    command
}

#[cfg(target_os = "macos")]
fn session_command() -> std::process::Command {
    let mut command = std::process::Command::new("/bin/launchctl");
    let uid = run_command(std::process::Command::new("/usr/bin/id").arg("-u"))
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned());
    let manager_uid = run_command(std::process::Command::new("/bin/launchctl").arg("manageruid"))
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned());
    if let (Some(uid), Some(manager_uid)) = (uid, manager_uid)
        && uid != manager_uid
        && !uid.is_empty()
        && uid.bytes().all(|b| b.is_ascii_digit())
    {
        // A LaunchDaemon with UserName= still inherits the system bootstrap
        // domain. Try the target user's domain without elevating the bridge.
        // macOS may reject this cross-domain operation; files remain valid.
        command.args(["asuser", &uid, "/bin/launchctl"]);
    }
    command
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn run_session_command(command: &mut std::process::Command) -> Option<std::process::Output> {
    match run_command(command) {
        Ok(output) if output.status.success() => Some(output),
        Ok(output) => {
            tracing::warn!(
                status = %output.status,
                stderr = %String::from_utf8_lossy(&output.stderr).trim(),
                "user session environment update unavailable; persistent environment files still apply"
            );
            None
        }
        Err(error) => {
            tracing::warn!(%error, "could not access user session environment; persistent environment files still apply");
            None
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn update_session_environment(url: Option<&str>) -> bool {
    let mut command = session_command();
    #[cfg(target_os = "linux")]
    match url {
        Some(url) => {
            command
                .arg("set-environment")
                .arg(format!("{VARIABLE}={url}"));
        }
        None => {
            command.arg("unset-environment").arg(VARIABLE);
        }
    }
    #[cfg(target_os = "macos")]
    match url {
        Some(url) => {
            command.arg("setenv").arg(VARIABLE).arg(url);
        }
        None => {
            command.arg("unsetenv").arg(VARIABLE);
        }
    }
    run_session_command(&mut command).is_some()
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn clear_session_environment_if_equal(url: &str) {
    let mut command = session_command();
    #[cfg(target_os = "linux")]
    command.arg("show-environment");
    #[cfg(target_os = "macos")]
    command.arg("getenv").arg(VARIABLE);
    let Some(output) = run_session_command(&mut command) else {
        return;
    };
    let output = String::from_utf8_lossy(&output.stdout);
    #[cfg(target_os = "linux")]
    let matches = output
        .lines()
        .any(|line| line == format!("{VARIABLE}={url}"));
    #[cfg(target_os = "macos")]
    let matches = output.trim_end_matches(['\r', '\n']) == url;
    if matches {
        update_session_environment(None);
    }
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn update_session_environment(_url: Option<&str>) -> bool {
    true
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn clear_session_environment_if_equal(_url: &str) {}

#[cfg(windows)]
fn broadcast_environment_change() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
    };
    let environment: Vec<u16> = "Environment\0".encode_utf16().collect();
    let mut result = 0usize;
    // SAFETY: The synchronous call borrows the valid NUL-terminated buffer and
    // output pointer for its duration. The timeout avoids a hung desktop window.
    let status = unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0,
            environment.as_ptr() as isize,
            SMTO_ABORTIFHUNG,
            2_000,
            &mut result,
        )
    };
    if status == 0 {
        tracing::warn!(
            "Windows environment broadcast did not complete; the registry value is saved"
        );
    }
}

#[cfg(windows)]
fn validate_sid(sid: &str) -> Result<()> {
    if !sid.starts_with("S-1-")
        || sid[4..]
            .split('-')
            .any(|part| part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()))
    {
        bail!("invalid Windows user SID");
    }
    Ok(())
}

/// The service installer saves this SID so LocalSystem can update the
/// installing user's HKU hive rather than LocalSystem's own HKCU.
pub fn current_user_sid() -> Result<Option<String>> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let output = run_command(
            std::process::Command::new("whoami.exe")
                .args(["/user", "/fo", "csv", "/nh"])
                .creation_flags(0x08000000),
        ) // CREATE_NO_WINDOW
        .context("query the installing user's Windows SID")?;
        if !output.status.success() {
            bail!(
                "whoami /user failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        // The account name can use the OEM codepage, while the SID is ASCII.
        let text = String::from_utf8_lossy(&output.stdout);
        let sid = text
            .split([',', '"', '\r', '\n'])
            .map(str::trim)
            .find(|part| part.starts_with("S-1-"))
            .context("whoami returned no user SID")?;
        validate_sid(sid)?;
        Ok(Some(sid.to_owned()))
    }
    #[cfg(not(windows))]
    {
        Ok(None)
    }
}
