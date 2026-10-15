# codex-app-server-bridge

English | [简体中文](README_ZH.md)

A Rust program that exposes Codex app-server at `ws://127.0.0.1:<port>/`. By default it starts one `codex app-server proxy` process per WebSocket client and carries the proxy's STDIO byte stream over WebSocket; it does not depend on Codex's control-socket path. An optional direct `socket` transport is available for compatibility. It can optionally set the user environment variable `CODEX_APP_SERVER_WS_URL`; this is disabled by default.

Supports Windows, macOS, and Linux, with fixed or dynamic ports and background services or login startup. It can optionally start the Codex app-server daemon when it is not running; this is disabled by default. In the default proxy mode, Codex owns discovery and lifecycle of its control endpoint.

## Downloads and automatic releases

Download an archive for your operating system and architecture from [GitHub Releases](https://github.com/chang196700/codex-app-server-bridge/releases). Each release includes `SHA256SUMS`, and each archive contains the executable, both READMEs, `LICENSE-MIT`, `LICENSE-APACHE`, and build information.

| Platform | Architectures | Archive |
| --- | --- | --- |
| Linux (GNU/glibc) | x86_64, ARM64 | `.tar.gz` |
| Linux (static musl) | x86_64, ARM64 | `.tar.gz` |
| macOS | Intel x86_64, Apple Silicon ARM64 | `.tar.gz` |
| Windows (MSVC) | x86_64, ARM64 | `.zip` |

All eight builds run on native GitHub-hosted runners. Linux GNU builds use Ubuntu 24.04, so those binaries target systems with glibc 2.39 or newer. The musl binaries are statically linked in Alpine containers on matching x86_64/ARM64 runners and do not require glibc; choose them for Alpine or older glibc distributions. The build verifies that musl executables have no dynamic interpreter or shared-library dependencies.

Pushing to `main` automatically runs [release-plz](https://release-plz.dev/) to update the version and generate `CHANGELOG.md` from Git commit messages, then commits those changes as `github-actions[bot]`. Automatic release commits are excluded from the changelog. If preparation is retried before the version is tagged, its changelog section is regenerated to include any intervening commits; tagged version sections are retained. After formatting, Clippy, release compilation, and an executable version check pass for all eight targets, the workflow creates a `v<version>` tag and publishes a GitHub Release with the complete set of archives and checksums. There is no manual release PR and no crates.io publication. Versions follow release-plz's commit analysis; use Conventional Commits such as `fix: ...`, `feat: ...`, and `feat!: ...` to communicate the change.

The workflow uses the built-in `GITHUB_TOKEN`; no additional secrets are required. `main` must allow the workflow's version commit. Its token-generated push does not start another workflow. Releases run serially; queued runs use the latest `main`, so closely spaced pushes can be included in one release. To retry an interrupted release, rerun the failed jobs or manually run the **Release** workflow on `main`. Existing tags keep their original source, and complete releases are not overwritten. Build failures leave the version commit on `main`, without publishing an incomplete release.

## Update from GitHub

```sh
# Update the installed user service or login startup binary
codex-app-server-bridge update

# Update only the executable running this command
codex-app-server-bridge update --self

# Locate a service installed with a custom configuration / state directory
codex-app-server-bridge --config /path/to/bridge.toml update
```

`update` checks the operating system's service or startup registration and updates the fixed executable under the selected user's `~/.codex-app-server-bridge` directory. If the service is not installed, it reports an error; a binary left behind by `service uninstall` does not count as an installation. The default scope is `user`.

The command reads the installed binary's version and downloads the latest stable GitHub Release for the updater's operating system and architecture. On Linux, a GNU updater selects the GNU/glibc archive and a musl updater selects the musl archive. It verifies the archive against the release's `SHA256SUMS` before replacing any executable. If the target version is already equal to or newer than the latest release, it leaves the target unchanged.

Service updates preserve the startup registration and the selected configuration file. A running service is stopped only after the download is validated, then restarted with the new binary and rereads that file; a stopped service remains stopped. If replacement or restart fails, the updater attempts to restore the previous binary and service state. Use the same `--user-home` and `--config` selection as the installation when locating a custom service.

For a Linux or macOS system service, supply the target username and home directory with the required administrator privileges:

```sh
# Linux; on macOS, use /Users/alice for the user's home directory
sudo /home/alice/.codex-app-server-bridge/codex-app-server-bridge \
  --user-home /home/alice update --scope system --username alice
```

For a Windows system service, run an elevated PowerShell session belonging to the target user:

```powershell
& 'C:\Users\alice\.codex-app-server-bridge\codex-app-server-bridge.exe' `
  --user-home 'C:\Users\alice' update --scope system
```

`update --self` replaces the executable used to invoke the command. It does not load configuration, require an installed service, or stop or restart a service, and cannot be combined with `--scope` or `--username`. It requires write access to the executable's directory. If you invoke it from the fixed service executable, that file is replaced, but an already running service keeps using its existing process until restarted. Use `update` to apply the new version to a running service automatically.

## Build and run

Install Rust / rustup and the native Rust build tools for your platform. The repository's `rust-toolchain.toml` pins Rust 1.99.0, the current stable release; native CI builds use the same version.

```sh
cargo build --release --locked
```

The executable is `target/release/codex-app-server-bridge`, or `codex-app-server-bridge.exe` in the same directory on Windows. The commands below assume it is on `PATH`; you can also use its absolute path.

```sh
# Create ~/.codex-app-server-bridge/config.toml without overwriting an existing file
codex-app-server-bridge init-config

# Run in the foreground; omitting the subcommand also runs the bridge
codex-app-server-bridge run

# Show the actual port, transport, PID, and readiness
codex-app-server-bridge status
```

The proxy or socket endpoint does not need to be ready at startup: the bridge stays running and periodically attempts to connect. Press `Ctrl+C` to stop a foreground instance.

```sh
# Fixed port; exits with an error if occupied, without switching ports
codex-app-server-bridge --port 8765 run

# Choose an available port dynamically
codex-app-server-bridge --port 0 run

# Specify the Codex data directory or configuration file
codex-app-server-bridge --codex-home /path/to/.codex run
codex-app-server-bridge --config /path/to/bridge.toml run
```

A dynamic port stays unchanged for the lifetime of the bridge process and may change after the bridge restarts. Restarting only Codex or its proxy endpoint does not change the port of a running bridge.

## Configuration

The default configuration is `~/.codex-app-server-bridge/config.toml`. The `.codex-app-server-bridge` directory also holds the executable installed for background startup. On Windows, `~` refers to the current user's home directory. Built-in defaults apply if the default file does not exist. Reading a missing file explicitly selected with `--config` is an error; `init-config` and `config set` can create it. `init-config` creates the directory and an example configuration without overwriting an existing configuration.

If `~/.codex-app-server-bridge` is still a legacy TOML file, selecting the default configuration automatically migrates its contents to `~/.codex-app-server-bridge/config.toml`. The original contents remain in a backup named `~/.codex-app-server-bridge.legacy-<unique-id>.toml.bak`. An explicit `--config` pointing to the old default path is also redirected to the new file. Invalid TOML files and symbolic links are not migrated. If migration is interrupted, the backup is retained and rerunning the command resumes recovery; conflicting file contents are not overwritten. Migration preserves the original configuration, so `init-config` still refuses to overwrite it afterward.

```toml
# Optional; defaults to CODEX_HOME, or ~/.codex if unset
# codex_home = "~/.codex"

# The default uses `codex app-server proxy` over STDIO, so the bridge does not
# depend on Codex's control-socket path. Set `transport = "socket"` only for direct access.
transport = "proxy"

# Used only when transport = "socket"; defaults to this path under codex_home
# socket_path = "~/.codex/app-server-control/app-server-control.sock"

auto_start_codex_app_server_daemon = false  # Start an unavailable daemon; disabled by default
# Optional; omit to find Codex on PATH or in common installation locations
# codex_path = "/absolute/path/to/codex"

poll_interval_ms = 2000         # Upstream health check interval; at least 100 ms

# Use a state directory dedicated to this program
# state_dir = "~/.local/state/codex-app-server-bridge"

[websocket]
host = "127.0.0.1"             # ::1 is also supported; only loopback addresses are accepted
port = 0                       # 0 selects a dynamic port; e.g. 8765 selects a fixed port
connect_timeout_ms = 3000       # Connection and handshake timeout; at least 100 ms
max_connections = 64
max_message_size = 67108864     # Bridge message/frame limit; Codex upstream limits still apply
publish_codex_app_server_ws_url_environment = false  # Disabled by default; true enables automatic user environment updates
```

WebSocket settings belong in the `[websocket]` table. Before upgrading a configuration that has top-level `host`, `port`, `connect_timeout_ms`, `max_connections`, `max_message_size`, or `publish_codex_app_server_ws_url_environment`, move those fields into `[websocket]`; the old locations are rejected and are not migrated automatically. Keep `poll_interval_ms`, transport, daemon, and path settings at the top level, before `[websocket]`.

Configuration precedence:

| Setting | Priority, highest first |
| --- | --- |
| Configuration file | `--config`; otherwise `.codex-app-server-bridge/config.toml` under `--user-home`, or the current OS user's home directory when omitted |
| User home directory | `--user-home` → TOML `user_home` → the current OS user's home directory |
| Codex directory | `--codex-home` → TOML `codex_home` → the `CODEX_HOME` environment variable → `.codex` in the selected user's home directory |
| Upstream transport | TOML `transport` → `proxy` (Codex `app-server proxy` over STDIO) |
| Socket path | Used only when `transport = "socket"`; TOML `socket_path` → `<codex_home>/app-server-control/app-server-control.sock` |
| Codex executable | TOML `codex_path` → automatic detection on `PATH` and in common installation locations |
| Port | `--port` → TOML `websocket.port` → `0` |

A TOML `user_home` value takes effect after the configuration is read; it does not change which configuration file is selected. A leading `~` expands to the selected user's home directory. Other relative paths resolve against the working directory from which the command is run. Service installation saves absolute paths. When editing a service's paths afterward, use absolute paths or `~` so they do not depend on the service manager's working directory. Unknown TOML fields are rejected so that typos are not silently ignored.

### Read and update configuration

```sh
codex-app-server-bridge config list
codex-app-server-bridge config get websocket.port
codex-app-server-bridge config set websocket.port 8765
codex-app-server-bridge config set websocket.publish_codex_app_server_ws_url_environment true
codex-app-server-bridge config set transport proxy
codex-app-server-bridge config set socket_path '~/.codex/app-server-control/app-server-control.sock'
codex-app-server-bridge config get auto_start_codex_app_server_daemon
codex-app-server-bridge config get codex_path

# Optional explicit executable; omit this command to use automatic detection
codex-app-server-bridge config set codex_path /absolute/path/to/codex
codex-app-server-bridge config set auto_start_codex_app_server_daemon true

# Read or edit a custom configuration file
codex-app-server-bridge --config /path/to/bridge.toml config get websocket.port
codex-app-server-bridge --config /path/to/bridge.toml config set websocket.port 8765
```

`config list` prints a JSON object containing the file's settings merged with built-in defaults. `config get KEY` prints only the selected value as JSON: strings include quotes, and an unset optional field is `null`. These commands show configuration values without applying runtime environment variables such as `CODEX_HOME` or CLI overrides such as `--port`.

`config set KEY VALUE` accepts a value matching the field's type. Numbers and booleans use TOML syntax; string fields, including paths and `websocket.host`, also accept plain strings as shown above. There is no special unset or `null` syntax for removing a field. The command creates a missing configuration file, including one selected with `--config`, preserves other fields and comments, validates the complete configuration before writing, and prints the saved value as JSON. It does not write runtime overrides into the file.

Configuration keys support dot-separated path navigation: use `websocket.port` for a field in `[websocket]`, and `transport` or `socket_path` for a top-level field. Unknown fields or subkeys are rejected.

The service reads the same configuration file that `config list`, `config get`, and `config set` select: the default `~/.codex-app-server-bridge/config.toml`, or the file supplied with `--config` at installation. Configuration changes do not reload a running instance. Restart a foreground process, or use `service restart` for an installed service; an operating-system service restart or reboot also applies the changes. Starting a stopped service reads the file again.

```sh
codex-app-server-bridge config set websocket.port 63100
codex-app-server-bridge service restart --scope user

# Use the same custom file for configuration and service commands
codex-app-server-bridge --config /path/to/bridge.toml config set auto_start_codex_app_server_daemon true
codex-app-server-bridge --config /path/to/bridge.toml service restart --scope user
```

Changes to `user_home` or `state_dir` affect installation or runtime locations. Stop the service before editing those fields, then run `service install` with the intended home and configuration selection; do not rely on a simple restart to relocate an installation.

## Start the Codex daemon automatically

Set `auto_start_codex_app_server_daemon = true` to let the bridge start an unavailable Codex app-server daemon. The option defaults to `false`. When enabled, the bridge creates a missing `CODEX_HOME` directory as needed before starting the daemon; it does not create that directory when the option is disabled. Install a Codex CLI version that supports `codex app-server daemon start`; the command has been checked with Codex CLI 0.162.1. Use the executable from a complete Codex CLI installation: copying only a standalone executable may leave it unable to prepare the daemon package.

The bridge runs its configured upstream health check. A healthy daemon is left running. If the proxy exits because the daemon is unavailable, or a direct socket connection returns `NotFound` or `ConnectionRefused`, a background worker runs the idempotent `codex app-server daemon start` command with the selected `CODEX_HOME`. Permission errors, protocol mismatches, and timeouts do not trigger a start. Startup attempts do not block the WebSocket listener; failures are logged and retried with an exponential delay from 5 to 60 seconds. Stopping the bridge does not stop the independent Codex daemon.

Omit `codex_path` to search `PATH` and common installation locations, or set it to the Codex executable's path. Absolute paths, a leading `~`, and relative paths follow the configuration path rules above. When automatic daemon startup is enabled, `service install` resolves the executable and saves its absolute path in the selected configuration file, preserving comments and other settings. After changing the option, `codex_path`, or `codex_home`, restart the bridge service to apply the change. If Codex moves, update `codex_path` before restarting.

On Unix, an npm-installed launcher also needs Node on the service's `PATH`. The bridge adds the selected Codex executable's directory to the startup command's `PATH`, supporting installations such as nvm that keep `node` alongside `codex`. If Node is unavailable there or on the service's existing `PATH`, set `codex_path` to the native executable within the complete Codex package.

With the default `transport = "proxy"`, automatic startup does not require the bridge to know the daemon's socket path; Codex resolves that endpoint. `transport = "socket"` retains direct control-socket compatibility and requires the standard `<codex_home>/app-server-control/app-server-control.sock` when automatic startup is enabled.

On Windows, automatic daemon startup requires an ordinary, non-elevated user process; use foreground mode or `service install --scope user` from a normal PowerShell session. Codex rejects daemon startup from elevated processes and LocalSystem. Installing a Windows `--scope system` service with automatic startup enabled is therefore rejected. A Windows system service can still bridge a daemon that is already running when this option is disabled. Linux and macOS system services run as the specified `--username` and can use automatic daemon startup.

## How environment updates work

Automatic environment updates are **disabled by default**, including when `websocket.publish_codex_app_server_ws_url_environment` is omitted. The WebSocket bridge still works, and `status` shows its URL. In this mode, the program does not modify user environment variables or shell configuration.

To set `CODEX_APP_SERVER_WS_URL` automatically, enable this option in `~/.codex-app-server-bridge/config.toml`:

```toml
[websocket]
publish_codex_app_server_ws_url_environment = true
```

If `[websocket]` already exists, add or update the field in that table instead of creating a second table.

After changing it, restart the foreground process or run `service restart` for an installed service. To disable an enabled publisher, first stop the old process so it can remove the variable it published, then set the option to `false` and start the bridge or service.

The following behavior applies only when publication is enabled. The bridge publishes the URL after connecting to the configured upstream and completing a WebSocket health check. If the upstream becomes unavailable, it withdraws the variable it published and republishes it when the upstream recovers. It also cleans up on normal exit. Cleanup checks the existing value and preserves a different value subsequently written by the user or another program.

| Platform | Publication method |
| --- | --- |
| Windows | The registry `Environment` key for the current user, or for the target user SID recorded during service installation, followed by an attempt to broadcast an environment change notification |
| Linux | `environment.sh` in the state directory, the user's `~/.config/environment.d/90-codex-app-server-bridge.conf`, and the systemd user manager's environment when accessible |
| macOS | `environment.sh` in the state directory and the target user's launchd environment when accessible |

On Unix, the program maintains marked source blocks in `.profile`, `.zprofile`, and any existing `.bash_profile` / `.bash_login`. It does not create `.bash_profile` for this purpose or alter unrelated configuration. Existing terminals and non-login shells using sh, Bash, or Zsh can load the file explicitly:

```sh
# Linux/macOS, with the default state directory; adjust for a custom state_dir
. "$HOME/.local/state/codex-app-server-bridge/environment.sh"
printf '%s\n' "$CODEX_APP_SERVER_WS_URL"
```

An existing Windows PowerShell session can reread the value from the user registry:

```powershell
$env:CODEX_APP_SERVER_WS_URL = [Environment]::GetEnvironmentVariable('CODEX_APP_SERVER_WS_URL', 'User')
```

**An external process cannot directly change the environment of an existing terminal or application.** Clients that are already running must reload their environment or restart. An inherited variable also remains in an existing process after the upstream goes offline. On Unix, `environment.sh` exists only while the URL is published. Do not treat a stale shell variable as current status when the upstream is offline; use `status` to check.

Native Windows services run in Session 0, so their environment change broadcasts cannot refresh Explorer in another session. The registry is still updated; the current session must explicitly read the variable, or inherit it after signing in again. Although a macOS system service runs as the target user, its launchd domain may still restrict GUI environment updates. Such failures are logged, and the shell environment file remains available. Conditions such as a Linux user manager that has not yet started are also logged, and session environment publication is retried during subsequent health checks.

## Install a service or enable startup

`service install` copies the current executable to `~/.codex-app-server-bridge/codex-app-server-bridge` (`codex-app-server-bridge.exe` on Windows), under the selected user's home directory, and registers this stable path for startup. You can install directly from a build or download directory and remove that source copy afterward. Keep the installed copy while the service or startup entry is in use. Use [`update`](#update-from-github) to upgrade the service from GitHub while retaining its configuration file. You can also run the same `service install` command using a new executable to update the installed copy and startup registration and restart the instance.

The installation directory follows the selected `user_home`, regardless of where `--config` points. Installation does not add the directory to `PATH`; use the installed executable's full path for later commands if needed. Running `service install` from the installed copy skips copying the executable onto itself and updates the configuration and startup registration. The installed copy can fetch a newer version with `update`.

`service install` registers and immediately starts the service or startup entry. Running it again updates the registration and restarts the instance. The registration points directly to the selected configuration file. Installation saves the necessary user identity, Codex directory, command-line overrides, and resolved Codex executable path to that same file while preserving comments and unrelated settings. Configured paths are made absolute for service use; omitted `socket_path` and `state_dir` remain derived defaults instead of being frozen into explicit values. Proxy services always resolve and save the selected `codex_path` so they can start under a minimal service environment. Changes to ordinary settings such as the port or daemon options take effect after `service restart`, without reinstalling. Startup modes sharing a state directory also share an instance lock. Do not install both user and system startup modes for the same state directory.

On Linux, the generated systemd unit stops only the bridge process, preserving the independent Codex daemon across bridge stops and restarts.

### Windows: start at user login

Run these commands in a normal PowerShell session as the target user; administrator privileges are not required. This writes to the current user's `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` and starts a hidden background process at the next login.

```powershell
$bridge = 'C:\Tools\codex-app-server-bridge.exe'
& $bridge service install --scope user
$bridge = Join-Path $HOME '.codex-app-server-bridge\codex-app-server-bridge.exe'
& $bridge service status --scope user
& $bridge service stop --scope user
& $bridge service start --scope user
& $bridge service restart --scope user
& $bridge service uninstall --scope user
```

Installation explicitly fails if the Run command exceeds 260 UTF-16 characters. Use a shorter configuration file path, or choose a system service. `stop` requests a graceful exit and waits for the instance lock to be released; it does not forcibly terminate the process.

### Windows: native system service

Run these commands in an **elevated administrator PowerShell session belonging to the target user**. Avoid installing as a different administrator account. Installation records the current user's SID. SCM runs the service as `LocalSystem`, and environment updates target the recorded user rather than LocalSystem's own user environment.

```powershell
$bridge = 'C:\Tools\codex-app-server-bridge.exe'
& $bridge --user-home 'C:\Users\alice' --codex-home 'C:\Users\alice\.codex' service install --scope system
$bridge = 'C:\Users\alice\.codex-app-server-bridge\codex-app-server-bridge.exe'
& $bridge service status --scope system
& $bridge service stop --scope system
& $bridge service start --scope system
& $bridge service restart --scope system
& $bridge service uninstall --scope system
```

Replace the example username and paths with your actual values. The service is named `codex-app-server-bridge` and uses delayed automatic startup. SCM attempts to restart it after an unexpected exit. `--username` does not apply on Windows.

### Linux: systemd user service

Run these commands while logged in as the target user:

```sh
codex-app-server-bridge service install --scope user
"$HOME/.codex-app-server-bridge/codex-app-server-bridge" service status --scope user
"$HOME/.codex-app-server-bridge/codex-app-server-bridge" service stop --scope user
"$HOME/.codex-app-server-bridge/codex-app-server-bridge" service start --scope user
"$HOME/.codex-app-server-bridge/codex-app-server-bridge" service restart --scope user
"$HOME/.codex-app-server-bridge/codex-app-server-bridge" service uninstall --scope user
```

The service file is `~/.config/systemd/user/codex-app-server-bridge.service`. To start the user manager before login and keep it running after logout, run the following as a user with the required privileges:

```sh
loginctl enable-linger alice
```

Installing and managing a user service requires an accessible systemd user manager. Do not use root's user session as a substitute for the target user when running `--scope user`.

### Linux: systemd system service

System installation requires an explicit target username and home directory. Specify the Codex directory as well to avoid changes to or removal of `CODEX_HOME` by `sudo`.

```sh
sudo ./target/release/codex-app-server-bridge \
  --user-home /home/alice --codex-home /home/alice/.codex \
  service install --scope system --username alice

sudo /home/alice/.codex-app-server-bridge/codex-app-server-bridge --user-home /home/alice --codex-home /home/alice/.codex service status --scope system --username alice
sudo /home/alice/.codex-app-server-bridge/codex-app-server-bridge --user-home /home/alice --codex-home /home/alice/.codex service stop --scope system --username alice
sudo /home/alice/.codex-app-server-bridge/codex-app-server-bridge --user-home /home/alice --codex-home /home/alice/.codex service start --scope system --username alice
sudo /home/alice/.codex-app-server-bridge/codex-app-server-bridge --user-home /home/alice service restart --scope system --username alice
sudo /home/alice/.codex-app-server-bridge/codex-app-server-bridge --user-home /home/alice --codex-home /home/alice/.codex service uninstall --scope system --username alice
```

The service file is `/etc/systemd/system/codex-app-server-bridge.service`. The process runs as `alice` and must be able to access the executable, selected configuration file, and Codex home. In proxy mode, the service starts Codex through its STDIO proxy and does not need a hard-coded socket path. Installation assigns the dedicated installation and state directories, installed executable, and selected configuration file to the target user; the executable has mode `0755`. Any parent directories created for this purpose within the user's home also belong to the target user. Existing parent directories retain their ownership and permissions. Installation does not recursively change permissions in the user home or Codex directory. The user's home directory must already exist.

### macOS: LaunchAgent at login

Run these commands in the target user's logged-in desktop session:

```sh
codex-app-server-bridge service install --scope user
"$HOME/.codex-app-server-bridge/codex-app-server-bridge" service status --scope user
"$HOME/.codex-app-server-bridge/codex-app-server-bridge" service stop --scope user
"$HOME/.codex-app-server-bridge/codex-app-server-bridge" service start --scope user
"$HOME/.codex-app-server-bridge/codex-app-server-bridge" service restart --scope user
"$HOME/.codex-app-server-bridge/codex-app-server-bridge" service uninstall --scope user
```

The configuration file is `~/Library/LaunchAgents/io.codex.app-server-bridge.plist`, registered in the current user's `gui/<uid>` domain. Without a GUI login session, use an appropriate system service setup instead.

### macOS: LaunchDaemon at boot

```sh
sudo ./target/release/codex-app-server-bridge \
  --user-home /Users/alice --codex-home /Users/alice/.codex \
  service install --scope system --username alice

sudo /Users/alice/.codex-app-server-bridge/codex-app-server-bridge --user-home /Users/alice --codex-home /Users/alice/.codex service status --scope system --username alice
sudo /Users/alice/.codex-app-server-bridge/codex-app-server-bridge --user-home /Users/alice --codex-home /Users/alice/.codex service stop --scope system --username alice
sudo /Users/alice/.codex-app-server-bridge/codex-app-server-bridge --user-home /Users/alice --codex-home /Users/alice/.codex service start --scope system --username alice
sudo /Users/alice/.codex-app-server-bridge/codex-app-server-bridge --user-home /Users/alice service restart --scope system --username alice
sudo /Users/alice/.codex-app-server-bridge/codex-app-server-bridge --user-home /Users/alice --codex-home /Users/alice/.codex service uninstall --scope system --username alice
```

The configuration file is `/Library/LaunchDaemons/io.codex.app-server-bridge.plist`. Its `UserName` setting runs the process as the specified user. If the user directory or Codex home is unavailable before login, the bridge still depends on those resources becoming available. A LaunchDaemon cannot unlock encrypted user data in advance.

### Uninstall and logs

`service uninstall` stops and removes the selected startup entry or service. The installed executable in `~/.codex-app-server-bridge`, `config.toml` (or your custom configuration file), and historical logs are retained for reinstallation or troubleshooting. A normal stop withdraws environment variables published by this instance. Cleanup cannot be guaranteed immediately after forced termination or a power loss; the next run uses the saved records to resume cleanup.

| Run mode | Logs |
| --- | --- |
| Foreground | Standard error |
| Windows background startup / system service | `<state_dir>/logs/bridge.log` |
| Linux user service | `journalctl --user -u codex-app-server-bridge -f` |
| Linux system service | `sudo journalctl -u codex-app-server-bridge -f` |
| macOS launchd | `<state_dir>/service.stdout.log` and `service.stderr.log` |

`status` checks the instance lock and reports runtime state, including the selected transport. `service status` queries the OS startup entry or service status. A running service does not necessarily mean the Codex upstream is ready; check the `ready` field from `status`.

## Protocol and access

The default implementation follows Codex's `app-server proxy`: [`stdio-to-uds`](https://github.com/openai/codex/tree/rust-v0.162.1/codex-rs/stdio-to-uds) forwards raw bytes between proxy STDIO and Codex's control endpoint, while [`unix_socket.rs`](https://github.com/openai/codex/blob/rust-v0.162.1/codex-rs/app-server-transport/src/transport/unix_socket.rs) handles the daemon-side WebSocket protocol. This bridge therefore uses **WebSocket over TCP ↔ WebSocket over proxy STDIO** by default and does not depend on a socket filename. With `transport = "socket"`, it uses the compatibility path **WebSocket over TCP ↔ WebSocket over UDS**. Neither mode parses the upstream as line-delimited JSON.

Each client connection has its own upstream connection; proxy mode starts and reaps one `codex app-server proxy` process for that client. Existing sessions may disconnect when the upstream process or endpoint closes. The bridge detects when it becomes available again, but clients must reconnect and reestablish any required Codex session themselves. Requests are not automatically replayed.

The bridge listens only on loopback addresses and accepts only the root path `/`. It rejects query parameters and handshakes containing `Origin`; `Host` must match the published URL. Other paths, such as `/daemon/shutdown`, are not forwarded. Connections from browser pages are rejected; use a native WebSocket client.

The bridge does not add token authentication. Loopback limits remote network access but does not isolate other users on the same machine. Run it in a trusted local environment, and use a controlled tunnel when remote access is needed. This implementation targets the Codex app-server WebSocket protocol exposed by proxy or the direct control endpoint. Other versions require revalidation if they change the handshake or protocol.

## License

Licensed under either the [MIT License](LICENSE-MIT) or the [Apache License 2.0](LICENSE-APACHE), at your option (`MIT OR Apache-2.0`).
