# codex-app-server-bridge

English | [简体中文](README_ZH.md)

A Rust program that monitors `$CODEX_HOME/app-server-control/app-server-control.sock` and exposes the local Codex control connection at `ws://127.0.0.1:<port>/`. It can optionally set the user environment variable `CODEX_APP_SERVER_WS_URL`; this is disabled by default.

Supports Windows, macOS, and Linux, with fixed or dynamic ports and background services or login startup. The program does not start Codex or create, delete, or replace its socket.

## Downloads and automatic releases

Download an archive for your operating system and architecture from [GitHub Releases](https://github.com/chang196700/codex-app-server-bridge/releases). Each release includes `SHA256SUMS`, and each archive contains the executable, both READMEs, and build information.

| Platform | Architectures | Archive |
| --- | --- | --- |
| Linux (GNU/glibc) | x86_64, ARM64 | `.tar.gz` |
| Linux (static musl) | x86_64, ARM64 | `.tar.gz` |
| macOS | Intel x86_64, Apple Silicon ARM64 | `.tar.gz` |
| Windows (MSVC) | x86_64, ARM64 | `.zip` |

All eight builds run on native GitHub-hosted runners. Linux GNU builds use Ubuntu 24.04, so those binaries target systems with glibc 2.39 or newer. The musl binaries are statically linked in Alpine containers on matching x86_64/ARM64 runners and do not require glibc; choose them for Alpine or older glibc distributions. The build verifies that musl executables have no dynamic interpreter or shared-library dependencies.

Pushing to `main` automatically runs [release-plz](https://release-plz.dev/) to update the version and `CHANGELOG.md`, then commits those changes as `github-actions[bot]`. After formatting, Clippy, release compilation, and an executable version check pass for all eight targets, the workflow creates a `v<version>` tag and publishes a GitHub Release with the complete set of archives and checksums. There is no manual release PR and no crates.io publication. Versions follow release-plz's commit analysis; use Conventional Commits such as `fix: ...`, `feat: ...`, and `feat!: ...` to communicate the change.

The workflow uses the built-in `GITHUB_TOKEN`; no additional secrets are required. `main` must allow the workflow's version commit. Its token-generated push does not start another workflow. Releases run serially; queued runs use the latest `main`, so closely spaced pushes can be included in one release. To retry an interrupted release, rerun the failed jobs or manually run the **Release** workflow on `main`. Existing tags keep their original source, and complete releases are not overwritten. Build failures leave the version commit on `main`, without publishing an incomplete release.

## Build and run

Install Rust / rustup and the native Rust build tools for your platform. The repository's `rust-toolchain.toml` specifies the toolchain version.

```sh
cargo build --release --locked
```

The executable is `target/release/codex-app-server-bridge`, or `codex-app-server-bridge.exe` in the same directory on Windows. The commands below assume it is on `PATH`; you can also use its absolute path.

```sh
# Create ~/.codex-app-server-bridge/config.toml without overwriting an existing file
codex-app-server-bridge init-config

# Run in the foreground; omitting the subcommand also runs the bridge
codex-app-server-bridge run

# Show the actual port, socket path, PID, and readiness
codex-app-server-bridge status
```

The socket does not need to exist at startup: the bridge stays running and periodically attempts to connect. Press `Ctrl+C` to stop a foreground instance.

```sh
# Fixed port; exits with an error if occupied, without switching ports
codex-app-server-bridge --port 8765 run

# Choose an available port dynamically
codex-app-server-bridge --port 0 run

# Specify the Codex data directory or configuration file
codex-app-server-bridge --codex-home /path/to/.codex run
codex-app-server-bridge --config /path/to/bridge.toml run
```

A dynamic port stays unchanged for the lifetime of the bridge process and may change after the bridge restarts. Restarting only Codex or recreating its socket does not change the port of a running bridge.

## Configuration

The default configuration is `~/.codex-app-server-bridge/config.toml`. The `.codex-app-server-bridge` directory also holds the executable installed for background startup. On Windows, `~` refers to the current user's home directory. Built-in defaults apply if the default file does not exist. Reading a missing file explicitly selected with `--config` is an error; `init-config` and `config set` can create it. `init-config` creates the directory and an example configuration without overwriting an existing configuration.

If `~/.codex-app-server-bridge` is still a legacy TOML file, selecting the default configuration automatically migrates its contents to `~/.codex-app-server-bridge/config.toml`. The original contents remain in a backup named `~/.codex-app-server-bridge.legacy-<unique-id>.toml.bak`. An explicit `--config` pointing to the old default path is also redirected to the new file. Invalid TOML files and symbolic links are not migrated. If migration is interrupted, the backup is retained and rerunning the command resumes recovery; conflicting file contents are not overwritten. Migration preserves the original configuration, so `init-config` still refuses to overwrite it afterward.

```toml
# Optional; defaults to CODEX_HOME, or ~/.codex if unset
# codex_home = "~/.codex"

# Usually unnecessary; defaults to the control socket path under codex_home
# socket_path = "~/.codex/app-server-control/app-server-control.sock"

host = "127.0.0.1"             # ::1 is also supported; only loopback addresses are accepted
port = 0                       # 0 selects a dynamic port; e.g. 8765 selects a fixed port
poll_interval_ms = 2000         # Socket health check interval; at least 100 ms
connect_timeout_ms = 3000       # Connection and handshake timeout; at least 100 ms
max_connections = 64
max_message_size = 67108864     # Bridge message/frame limit; Codex upstream limits still apply
publish_codex_app_server_ws_url_environment = false  # Disabled by default; true enables automatic user environment updates

# Use a state directory dedicated to this program
# state_dir = "~/.local/state/codex-app-server-bridge"
```

Configuration precedence:

| Setting | Priority, highest first |
| --- | --- |
| Configuration file | `--config`; otherwise `.codex-app-server-bridge/config.toml` under `--user-home`, or the current OS user's home directory when omitted |
| User home directory | `--user-home` → TOML `user_home` → the current OS user's home directory |
| Codex directory | `--codex-home` → TOML `codex_home` → the `CODEX_HOME` environment variable → `.codex` in the selected user's home directory |
| Socket path | TOML `socket_path` → `<codex_home>/app-server-control/app-server-control.sock` |
| Port | `--port` → TOML `port` → `0` |

A TOML `user_home` value takes effect after the configuration is read; it does not change which configuration file is selected. A leading `~` expands to the selected user's home directory. Other relative paths resolve against the working directory from which the command is run. Service installation saves absolute paths. Unknown TOML fields are rejected so that typos are not silently ignored.

### Read and update configuration

```sh
codex-app-server-bridge config list
codex-app-server-bridge config get port
codex-app-server-bridge config set port 8765
codex-app-server-bridge config set publish_codex_app_server_ws_url_environment true
codex-app-server-bridge config set socket_path '~/.codex/app-server-control/app-server-control.sock'

# Read or edit a custom configuration file
codex-app-server-bridge --config /path/to/bridge.toml config get port
codex-app-server-bridge --config /path/to/bridge.toml config set port 8765
```

`config list` prints a JSON object containing the file's settings merged with built-in defaults. `config get KEY` prints only the selected value as JSON: strings include quotes, and an unset optional field is `null`. These commands show configuration values without applying runtime environment variables such as `CODEX_HOME` or CLI overrides such as `--port`.

`config set KEY VALUE` accepts a value matching the field's type. Numbers and booleans use TOML syntax; string fields, including paths and `host`, also accept plain strings as shown above. There is no special unset or `null` syntax for removing a field. The command creates a missing configuration file, including one selected with `--config`, preserves other fields and comments, validates the complete configuration before writing, and prints the saved value as JSON. It does not write runtime overrides into the file.

Configuration keys support dot-separated path navigation. The current schema has only top-level fields, so use names such as `port` and `socket_path`; there are no nested settings to address yet. Unknown fields or subkeys are rejected.

Configuration changes do not reload a running instance. Restart a foreground process to apply them. For an installed service, rerun the same `service install` command to refresh `<state_dir>/service.toml` and restart the service; `service start` alone keeps using the existing snapshot.

## How environment updates work

Automatic environment updates are **disabled by default**, including when `publish_codex_app_server_ws_url_environment` is omitted. The WebSocket bridge still works, and `status` shows its URL. In this mode, the program does not modify user environment variables or shell configuration.

If an existing configuration uses the old name `publish_environment`, replace it with `publish_codex_app_server_ws_url_environment`. For an installed service, also rerun `service install` to update its configuration snapshot.

To set `CODEX_APP_SERVER_WS_URL` automatically, enable this option in `~/.codex-app-server-bridge/config.toml`:

```toml
publish_codex_app_server_ws_url_environment = true
```

After changing it, restart the foreground process. For an installed service, rerun the same `service install` command to update the configuration snapshot and restart the service. To disable an enabled publisher, first stop the old process so it can remove the variable it published, then set the option to `false` and start the bridge or reinstall the service.

The following behavior applies only when publication is enabled. The bridge publishes the URL after connecting to the socket and completing a WebSocket health check. If the socket becomes unavailable, it withdraws the variable it published and republishes it when the socket recovers. It also cleans up on normal exit. Cleanup checks the existing value and preserves a different value subsequently written by the user or another program.

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

**An external process cannot directly change the environment of an existing terminal or application.** Clients that are already running must reload their environment or restart. An inherited variable also remains in an existing process after the socket goes offline. On Unix, `environment.sh` exists only while the URL is published. Do not treat a stale shell variable as current status when the socket is offline; use `status` to check.

Native Windows services run in Session 0, so their environment change broadcasts cannot refresh Explorer in another session. The registry is still updated; the current session must explicitly read the variable, or inherit it after signing in again. Although a macOS system service runs as the target user, its launchd domain may still restrict GUI environment updates. Such failures are logged, and the shell environment file remains available. Conditions such as a Linux user manager that has not yet started are also logged, and session environment publication is retried during subsequent health checks.

## Install a service or enable startup

`service install` copies the current executable to `~/.codex-app-server-bridge/codex-app-server-bridge` (`codex-app-server-bridge.exe` on Windows), under the selected user's home directory, and registers this stable path for startup. You can install directly from a build or download directory and remove that source copy afterward. Keep the installed copy while the service or startup entry is in use. To upgrade, run the same `service install` command using the new executable; installation refreshes the installed copy and configuration snapshot and restarts the instance.

The installation directory follows the selected `user_home`, regardless of where `--config` points. Installation does not add the directory to `PATH`; use the installed executable's full path for later commands if needed. Running `service install` from the installed copy skips copying the executable onto itself and updates the configuration and startup registration. To install a newer version, run `service install` from the new build or download instead.

`service install` registers and immediately starts the service or startup entry. Running it again updates the configuration and restarts the instance. Installation saves the effective configuration to `<state_dir>/service.toml`, which the background process reads. Changes to the original configuration, port, or `CODEX_HOME` require another `service install` to take effect; `service start` alone does not regenerate the snapshot. Startup modes sharing a state directory also share an instance lock. Do not install both user and system startup modes for the same state directory.

### Windows: start at user login

Run these commands in a normal PowerShell session as the target user; administrator privileges are not required. This writes to the current user's `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` and starts a hidden background process at the next login.

```powershell
$bridge = 'C:\Tools\codex-app-server-bridge.exe'
& $bridge service install --scope user
$bridge = Join-Path $HOME '.codex-app-server-bridge\codex-app-server-bridge.exe'
& $bridge service status --scope user
& $bridge service stop --scope user
& $bridge service start --scope user
& $bridge service uninstall --scope user
```

Installation explicitly fails if the Run command exceeds 260 UTF-16 characters. Use a shorter state directory path, or choose a system service. `stop` requests a graceful exit and waits for the instance lock to be released; it does not forcibly terminate the process.

### Windows: native system service

Run these commands in an **elevated administrator PowerShell session belonging to the target user**. Avoid installing as a different administrator account. Installation records the current user's SID. SCM runs the service as `LocalSystem`, and environment updates target the recorded user rather than LocalSystem's own user environment.

```powershell
$bridge = 'C:\Tools\codex-app-server-bridge.exe'
& $bridge --user-home 'C:\Users\alice' --codex-home 'C:\Users\alice\.codex' service install --scope system
$bridge = 'C:\Users\alice\.codex-app-server-bridge\codex-app-server-bridge.exe'
& $bridge service status --scope system
& $bridge service stop --scope system
& $bridge service start --scope system
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
sudo /home/alice/.codex-app-server-bridge/codex-app-server-bridge --user-home /home/alice --codex-home /home/alice/.codex service uninstall --scope system --username alice
```

The service file is `/etc/systemd/system/codex-app-server-bridge.service`. The process runs as `alice` and must be able to access the executable, configuration snapshot, and Codex socket. Installation assigns the dedicated installation and state directories, installed executable, and configuration snapshot to the target user; the executable has mode `0755`. Any parent directories created for this purpose within the user's home also belong to the target user. Existing parent directories retain their ownership and permissions. Installation does not recursively change permissions in the user home or Codex directory. The user's home directory must already exist.

### macOS: LaunchAgent at login

Run these commands in the target user's logged-in desktop session:

```sh
codex-app-server-bridge service install --scope user
"$HOME/.codex-app-server-bridge/codex-app-server-bridge" service status --scope user
"$HOME/.codex-app-server-bridge/codex-app-server-bridge" service stop --scope user
"$HOME/.codex-app-server-bridge/codex-app-server-bridge" service start --scope user
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
sudo /Users/alice/.codex-app-server-bridge/codex-app-server-bridge --user-home /Users/alice --codex-home /Users/alice/.codex service uninstall --scope system --username alice
```

The configuration file is `/Library/LaunchDaemons/io.codex.app-server-bridge.plist`. Its `UserName` setting runs the process as the specified user. If the user directory or Codex socket is unavailable before login, the bridge still depends on those resources becoming available. A LaunchDaemon cannot unlock encrypted user data in advance.

### Uninstall and logs

`service uninstall` stops and removes the selected startup entry or service. The installed executable in `~/.codex-app-server-bridge`, `config.toml` (or your custom configuration file), configuration snapshot, and historical logs are retained for reinstallation or troubleshooting. A normal stop withdraws environment variables published by this instance. Cleanup cannot be guaranteed immediately after forced termination or a power loss; the next run uses the saved records to resume cleanup.

| Run mode | Logs |
| --- | --- |
| Foreground | Standard error |
| Windows background startup / system service | `<state_dir>/logs/bridge.log` |
| Linux user service | `journalctl --user -u codex-app-server-bridge -f` |
| Linux system service | `sudo journalctl -u codex-app-server-bridge -f` |
| macOS launchd | `<state_dir>/service.stdout.log` and `service.stderr.log` |

`status` checks the instance lock and reports runtime state. `service status` queries the OS startup entry or service status. A running service does not necessarily mean the Codex socket is ready; check the `ready` field from `status`.

## Protocol and access

The implementation follows the official OpenAI Codex `rust-v0.162.1` source: [`unix_socket.rs`](https://github.com/openai/codex/blob/rust-v0.162.1/codex-rs/app-server-transport/src/transport/unix_socket.rs) performs a WebSocket handshake over UDS, and [`codex-rs/uds/src/lib.rs`](https://github.com/openai/codex/blob/rust-v0.162.1/codex-rs/uds/src/lib.rs) provides the cross-platform UDS implementation. This bridge therefore uses **WebSocket over TCP ↔ WebSocket over UDS**. It does not parse the socket as line-delimited JSON.

Each client connection has its own upstream connection, forwarding text and binary messages. Existing sessions may disconnect when the socket is closed or replaced. The bridge detects when it becomes available again, but clients must reconnect and reestablish any required Codex session themselves. Requests are not automatically replayed.

The bridge listens only on loopback addresses and accepts only the root path `/`. It rejects query parameters and handshakes containing `Origin`; `Host` must match the published URL. Other paths, such as `/daemon/shutdown`, are not forwarded. Connections from browser pages are rejected; use a native WebSocket client.

The bridge does not add token authentication. Loopback limits remote network access but does not isolate other users on the same machine. Run it in a trusted local environment, and use a controlled tunnel when remote access is needed. This implementation targets the Codex control socket protocol described above. Other versions require revalidation if they change the handshake or protocol.
