//! Opt-in supervision of Codex's independently managed app-server daemon.

use crate::{codex_path, config::Settings};
use anyhow::{Context, Result, bail};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::{
    process::Command,
    sync::watch,
    time::{Instant, MissedTickBehavior, interval, timeout},
};

const INITIAL_RETRY: Duration = Duration::from_secs(5);
const MAX_RETRY: Duration = Duration::from_secs(60);
// Codex can wait up to 375 seconds for its lifecycle lock, in addition to
// preparing its managed installation and waiting for daemon readiness.
const START_TIMEOUT: Duration = Duration::from_secs(420);
const OUTPUT_LIMIT: u64 = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    Ready,
    Absent,
    Unavailable,
}

impl State {
    pub fn from_probe(result: &Result<()>) -> Self {
        match result {
            Ok(()) => Self::Ready,
            Err(error)
                if error.chain().any(|cause| {
                    cause.downcast_ref::<std::io::Error>().is_some_and(|error| {
                        matches!(
                            error.kind(),
                            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                        )
                    })
                }) =>
            {
                Self::Absent
            }
            // An inaccessible socket, an unresponsive server, and a protocol
            // mismatch are not proof that the daemon has stopped.
            Err(_) => Self::Unavailable,
        }
    }
}

/// This worker is independent of socket readiness probing and client handling.
/// The daemon belongs to Codex and is not stopped when this worker exits.
pub async fn run(settings: Arc<Settings>, mut health: watch::Receiver<State>) {
    if !settings.config.auto_start_codex_app_server_daemon {
        return;
    }
    if let Err(error) = check_launch_context() {
        tracing::error!(%error, "automatic Codex daemon startup is unavailable in this process");
        return;
    }
    let mut ticker = interval(Duration::from_millis(settings.config.poll_interval_ms));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut retry_delay = INITIAL_RETRY;
    let mut next_attempt = Instant::now();
    loop {
        tokio::select! {
            _ = ticker.tick() => {},
            changed = health.changed() => if changed.is_err() { break; },
        }
        let state = *health.borrow_and_update();
        if state == State::Ready {
            retry_delay = INITIAL_RETRY;
            next_attempt = Instant::now();
            continue;
        }
        if state != State::Absent || Instant::now() < next_attempt {
            continue;
        }
        match start(&settings).await {
            Ok(()) => {
                tracing::info!("Codex daemon start completed; checking control socket readiness")
            }
            Err(error) => {
                tracing::warn!(error = %format!("{error:#}"), retry_seconds = retry_delay.as_secs(), "could not start Codex daemon; will retry")
            }
        }
        next_attempt = Instant::now() + retry_delay;
        retry_delay = (retry_delay * 2).min(MAX_RETRY);
    }
}

async fn start(settings: &Settings) -> Result<()> {
    let executable =
        codex_path::resolve(settings.config.codex_path.as_deref(), &settings.user_home)?;
    // An explicitly supplied CODEX_HOME must already exist for the Codex CLI.
    // Create it only after startup was enabled and a valid executable was found.
    crate::config::private_dir(&settings.codex_home).context("create Codex home for daemon")?;
    // Use files rather than pipes: a detached Windows descendant can inherit
    // handles, preventing a pipe reader from observing EOF after the CLI exits.
    let mut stdout = tempfile::tempfile().context("create daemon command output file")?;
    let mut stderr = tempfile::tempfile().context("create daemon command error file")?;
    let mut command = Command::new(&executable);
    // npm's Unix launcher needs node. Version managers commonly install both
    // launchers together, while service managers supply only a minimal PATH.
    if let Some(directory) = executable.parent() {
        let mut paths = vec![directory.to_path_buf()];
        if let Some(path) = std::env::var_os("PATH") {
            paths.extend(std::env::split_paths(&path));
        }
        command.env(
            "PATH",
            std::env::join_paths(paths).context("prepare Codex command PATH")?,
        );
    }
    command
        .args(["app-server", "daemon", "start"])
        .env("CODEX_HOME", &settings.codex_home)
        .env("HOME", &settings.user_home)
        .env_remove("CODEX_APP_SERVER_WS_URL")
        .current_dir(&settings.user_home)
        .stdin(Stdio::null())
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?)
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    tracing::info!(codex = %executable.display(), codex_home = %settings.codex_home.display(), "control socket absent; starting Codex app-server daemon");
    let mut child = command
        .spawn()
        .with_context(|| format!("launch {} app-server daemon start", executable.display()))?;
    let status = match timeout(START_TIMEOUT, child.wait()).await {
        Ok(result) => result.context("wait for Codex daemon start command")?,
        Err(_) => {
            let _ = child.kill().await;
            bail!(
                "Codex daemon start command timed out after {} seconds",
                START_TIMEOUT.as_secs()
            );
        }
    };
    if !status.success() {
        let error_text = read_output(&mut stderr)?;
        let output_text = read_output(&mut stdout)?;
        bail!(
            "Codex daemon start exited with {status}: {}",
            if error_text.trim().is_empty() {
                output_text.trim()
            } else {
                error_text.trim()
            }
        );
    }
    // A successful CLI exit is only a launch acknowledgement. The independent
    // WebSocket probe, not this exit code or a PID file, controls bridge readiness.
    Ok(())
}

fn read_output(file: &mut File) -> Result<String> {
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    file.take(OUTPUT_LIMIT).read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Codex's Windows daemon refuses elevated launches. Enforce this before
/// executing a user-configured file, especially from a LocalSystem service.
pub fn check_launch_context() -> Result<()> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::{
            Foundation::CloseHandle,
            Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation},
            System::Threading::{GetCurrentProcess, OpenProcessToken},
        };
        let mut token = std::ptr::null_mut();
        // SAFETY: token receives an owned handle, closed immediately after the
        // query; both query buffers are correctly sized live stack values.
        unsafe {
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return Err(std::io::Error::last_os_error())
                    .context("inspect daemon launch identity");
            }
            let mut elevation: TOKEN_ELEVATION = std::mem::zeroed();
            let mut length = 0;
            let success = GetTokenInformation(
                token,
                TokenElevation,
                (&mut elevation as *mut TOKEN_ELEVATION).cast(),
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut length,
            );
            let error = std::io::Error::last_os_error();
            CloseHandle(token);
            if success == 0 {
                return Err(error).context("inspect daemon launch elevation");
            }
            if elevation.TokenIsElevated != 0 {
                bail!(
                    "Codex daemon auto-start requires a non-elevated Windows user; use current-user login startup, not an administrator terminal or a LocalSystem service"
                );
            }
        }
    }
    Ok(())
}
