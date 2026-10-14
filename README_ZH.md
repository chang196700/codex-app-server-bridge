# codex-app-server-bridge

[English](README.md) | 简体中文

使用 Rust 监控 `$CODEX_HOME/app-server-control/app-server-control.sock`，将 Codex 的本地控制连接转发为 `ws://127.0.0.1:<端口>/`。可选开启自动设置用户环境变量 `CODEX_APP_SERVER_WS_URL`，默认关闭。

支持 Windows、macOS 和 Linux；支持固定端口、动态端口，以及后台服务或登录启动。程序不会启动 Codex，也不会创建、删除或替换 Codex 的 socket。

## 构建与运行

安装 Rust / rustup 和当前平台的 Rust 原生构建工具。工具链版本由仓库中的 `rust-toolchain.toml` 确定。

```sh
cargo build --release --locked
```

构建产物为 `target/release/codex-app-server-bridge`；Windows 为同目录下的 `codex-app-server-bridge.exe`。以下命令假设程序已加入 `PATH`；也可以直接使用产物的绝对路径。

```sh
# 创建 ~/.codex-app-server-bridge/config.toml，已有文件不会被覆盖
codex-app-server-bridge init-config

# 前台运行；不带子命令也等同于 run
codex-app-server-bridge run

# 查看实际端口、socket 路径、PID 和就绪状态
codex-app-server-bridge status
```

启动时 socket 尚不存在也可以：桥接会保持运行并定期尝试连接。前台使用 `Ctrl+C` 退出。

```sh
# 固定端口；端口被占用会报错退出，不会悄悄改用其他端口
codex-app-server-bridge --port 8765 run

# 动态分配一个可用端口
codex-app-server-bridge --port 0 run

# 指定 Codex 数据目录或配置文件
codex-app-server-bridge --codex-home /path/to/.codex run
codex-app-server-bridge --config /path/to/bridge.toml run
```

动态端口在一次运行期间保持不变；桥接进程重启后可能变化。只重启 Codex、重新创建 socket，不会使仍在运行的桥接更换监听端口。

## 配置

默认配置为 `~/.codex-app-server-bridge/config.toml`。`.codex-app-server-bridge` 目录也用于存放为后台启动安装的程序副本。Windows 中 `~` 对应当前用户主目录。默认文件不存在时使用内置配置。读取显式 `--config` 指定的文件时，文件不存在则报错；`init-config` 和 `config set` 可以创建该文件。`init-config` 会创建目录及示例配置，不覆盖已有配置。

如果 `~/.codex-app-server-bridge` 仍是旧版 TOML 文件，选用默认配置时会自动将其内容迁移到 `~/.codex-app-server-bridge/config.toml`，并将原内容保留为 `~/.codex-app-server-bridge.legacy-<唯一标识>.toml.bak`。显式 `--config` 指向旧默认路径时也会转为使用新文件。无效 TOML 文件和符号链接不会迁移。迁移中断时会保留备份，重新执行命令可继续恢复，不覆盖内容冲突的文件。迁移保留原配置，因此随后执行 `init-config` 仍会拒绝覆盖已有文件。

```toml
# 可省略；默认使用 CODEX_HOME，未设置时使用 ~/.codex
# codex_home = "~/.codex"

# 通常不需要配置；默认拼接 codex_home 下的控制 socket 路径
# socket_path = "~/.codex/app-server-control/app-server-control.sock"

host = "127.0.0.1"             # 也可使用 ::1；只接受 loopback 地址
port = 0                       # 0 为动态端口；例如 8765 为固定端口
poll_interval_ms = 2000         # socket 健康检查间隔，至少 100 毫秒
connect_timeout_ms = 3000       # 连接和握手超时，至少 100 毫秒
max_connections = 64
max_message_size = 67108864     # 本桥接的消息/帧上限；仍受 Codex 上游限制
publish_codex_app_server_ws_url_environment = false  # 默认关闭；设为 true 自动设置用户环境变量

# 必须使用本程序专用的状态目录
# state_dir = "~/.local/state/codex-app-server-bridge"
```

配置优先级：

| 项目 | 优先级，从高到低 |
| --- | --- |
| 配置文件 | `--config`；否则 `--user-home` 或操作系统当前用户主目录下的 `.codex-app-server-bridge/config.toml` |
| 用户主目录 | `--user-home` → TOML `user_home` → 操作系统当前用户主目录 |
| Codex 目录 | `--codex-home` → TOML `codex_home` → 环境变量 `CODEX_HOME` → 所选用户主目录下的 `.codex` |
| socket 路径 | TOML `socket_path` → `<codex_home>/app-server-control/app-server-control.sock` |
| 端口 | `--port` → TOML `port` → `0` |

TOML 中的 `user_home` 在读取配置后生效，不会改变已经选定的配置文件路径。路径开头的 `~` 会展开为所选用户主目录；其他相对路径相对于启动命令时的工作目录。安装服务时会将路径固定为绝对路径。TOML 未知字段会报错，避免拼写错误被忽略。

### 查看和修改配置

```sh
codex-app-server-bridge config list
codex-app-server-bridge config get port
codex-app-server-bridge config set port 8765
codex-app-server-bridge config set publish_codex_app_server_ws_url_environment true
codex-app-server-bridge config set socket_path '~/.codex/app-server-control/app-server-control.sock'

# 查看或修改自定义配置文件
codex-app-server-bridge --config /path/to/bridge.toml config get port
codex-app-server-bridge --config /path/to/bridge.toml config set port 8765
```

`config list` 输出配置文件与内置默认值合并后的 JSON 对象。`config get KEY` 仅以 JSON 输出指定字段的值：字符串带引号，未设置的可选字段为 `null`。这些命令显示配置值，不应用 `CODEX_HOME` 等运行时环境变量或 `--port` 等命令行覆盖。

`config set KEY VALUE` 接受与字段类型匹配的值。数字和布尔值使用 TOML 语法；路径、`host` 等字符串字段也可直接传入裸字符串，如上方示例。没有用于删除字段的特殊 unset 或 `null` 语法。该命令会创建缺失的配置文件，包括显式 `--config` 指定的文件；保留其他字段和注释，校验完整配置后再写入，并以 JSON 输出保存后的值，不会将运行时覆盖值写回文件。

配置键支持使用点分路径导航。当前配置结构只有顶层字段，因此直接使用 `port`、`socket_path` 等字段名；目前没有可通过点分路径访问的嵌套配置项。未知字段或子键会被拒绝。

修改配置不会使运行中的实例自动重新加载。前台进程需要重启；已安装的服务需要重新执行相同的 `service install` 命令，更新 `<state_dir>/service.toml` 并重启服务。单独执行 `service start` 仍会使用原有快照。

## 环境变量如何生效

自动设置环境变量**默认关闭**，省略 `publish_codex_app_server_ws_url_environment` 也保持关闭。此时 WebSocket 桥接正常运行，可通过 `status` 查看 URL，程序不会修改用户环境变量或 shell 配置。

已有配置若使用旧名称 `publish_environment`，请将其替换为 `publish_codex_app_server_ws_url_environment`；已安装的服务还需重新运行 `service install` 更新配置快照。

需要自动设置 `CODEX_APP_SERVER_WS_URL` 时，在 `~/.codex-app-server-bridge/config.toml` 中开启：

```toml
publish_codex_app_server_ws_url_environment = true
```

修改后重启前台进程；已安装服务则重新运行相同的 `service install` 命令，使配置快照更新并重启。要关闭已开启的发布功能，先停止原进程，让它清理自己发布的变量，再设为 `false` 并启动或重新安装服务。

以下发布行为仅在开启时生效：桥接完成 socket 连接和 WebSocket 健康检查后才发布 URL。socket 失联时撤回自己发布的变量，恢复后重新发布；正常退出时也会清理。清理会核对原值，保留用户或其他程序后来写入的不同值。

| 平台 | 发布方式 |
| --- | --- |
| Windows | 当前用户或服务安装时记录的目标用户 SID 对应的注册表 `Environment` 项，并尝试发送环境变更通知 |
| Linux | 状态目录下的 `environment.sh`、用户 `~/.config/environment.d/90-codex-app-server-bridge.conf`，以及可访问时的 systemd 用户管理器环境 |
| macOS | 状态目录下的 `environment.sh`，以及可访问时的目标用户 launchd 环境 |

Unix 平台会在 `.profile`、`.zprofile`，以及已经存在的 `.bash_profile` / `.bash_login` 中维护带标记的加载段。不会为此新建 `.bash_profile`，也不会改写无关配置。使用 sh、Bash 或 Zsh 的现有终端、非登录 shell 可以显式加载：

```sh
# Linux/macOS，默认状态目录；自定义 state_dir 时相应调整
. "$HOME/.local/state/codex-app-server-bridge/environment.sh"
printf '%s\n' "$CODEX_APP_SERVER_WS_URL"
```

Windows 当前 PowerShell 可以从用户注册表重新读取：

```powershell
$env:CODEX_APP_SERVER_WS_URL = [Environment]::GetEnvironmentVariable('CODEX_APP_SERVER_WS_URL', 'User')
```

**已有终端和应用的环境不会被外部进程直接改变。** 已经启动的客户端需要重新加载环境或重启；socket 离线后，旧进程中继承的变量也不会自动消失。Unix 的 `environment.sh` 只有在 URL 发布期间存在，离线时不要将旧 shell 变量当作实时状态；可运行 `status` 确认。

Windows 原生服务运行在 Session 0，其环境变更广播无法刷新另一会话中的 Explorer。注册表仍会更新；当前会话需要主动读取变量，或重新登录后继承。macOS 系统服务虽按目标用户身份运行，其 launchd 域仍可能限制 GUI 环境更新；这类失败会记录日志，shell 环境文件仍可使用。Linux 用户管理器尚未启动等情况也会记录日志，并在后续健康检查中重试会话环境发布。

## 安装服务或开机启动

`service install` 会将当前程序复制到所选用户主目录下的 `~/.codex-app-server-bridge/codex-app-server-bridge`（Windows 为 `codex-app-server-bridge.exe`），并将这个稳定路径注册为启动程序。可以直接从构建目录或下载目录安装，安装后可删除来源文件；服务或启动项使用期间需保留安装目录中的副本。升级时，使用新版程序再次执行相同的 `service install` 命令，安装会更新程序副本和配置快照并重启实例。

安装目录由所选 `user_home` 决定，不随 `--config` 的位置改变。安装不会自动将目录加入 `PATH`，后续可使用安装副本的完整路径执行命令。从安装副本本身执行 `service install` 时会跳过自复制，更新配置及启动注册；升级版本时应从新构建或下载的程序执行 `service install`。

`service install` 会注册并立即启动；重复安装会更新配置并重新启动。安装将当前有效配置保存到 `<state_dir>/service.toml`，后台进程读取此快照。修改原始配置、端口或 `CODEX_HOME` 后，需要再次运行 `service install` 才会应用；单独 `service start` 不会重新生成快照。不同启动方式共用同一状态目录的实例锁，不应同时安装用户和系统两种启动方式。

### Windows：当前用户登录启动

在目标用户的普通 PowerShell 中执行，不需要管理员权限。此方式写入当前用户的 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`，下次登录后启动隐藏的后台进程。

```powershell
$bridge = 'C:\Tools\codex-app-server-bridge.exe'
& $bridge service install --scope user
$bridge = Join-Path $HOME '.codex-app-server-bridge\codex-app-server-bridge.exe'
& $bridge service status --scope user
& $bridge service stop --scope user
& $bridge service start --scope user
& $bridge service uninstall --scope user
```

Run 启动命令超过 260 个 UTF-16 字符时安装会明确失败；请使用更短的状态目录路径，或选择系统服务。`stop` 会请求正常退出并等待实例锁释放，不使用强制结束进程。

### Windows：原生系统服务

在**目标用户本人提升权限的管理员 PowerShell**中执行，避免使用另一个管理员账户安装。安装会记录当前用户 SID；服务由 SCM 以 `LocalSystem` 运行，环境变量写入记录的用户，而不是 LocalSystem 自己的用户环境。

```powershell
$bridge = 'C:\Tools\codex-app-server-bridge.exe'
& $bridge --user-home 'C:\Users\alice' --codex-home 'C:\Users\alice\.codex' service install --scope system
$bridge = 'C:\Users\alice\.codex-app-server-bridge\codex-app-server-bridge.exe'
& $bridge service status --scope system
& $bridge service stop --scope system
& $bridge service start --scope system
& $bridge service uninstall --scope system
```

将示例用户名和路径替换为真实值。服务名为 `codex-app-server-bridge`，配置为延迟自动启动，异常退出后由 SCM 尝试重新启动。Windows 的 `--username` 不适用。

### Linux：systemd 用户服务

以目标用户登录后执行：

```sh
codex-app-server-bridge service install --scope user
"$HOME/.codex-app-server-bridge/codex-app-server-bridge" service status --scope user
"$HOME/.codex-app-server-bridge/codex-app-server-bridge" service stop --scope user
"$HOME/.codex-app-server-bridge/codex-app-server-bridge" service start --scope user
"$HOME/.codex-app-server-bridge/codex-app-server-bridge" service uninstall --scope user
```

服务文件为 `~/.config/systemd/user/codex-app-server-bridge.service`。要在用户尚未登录时也启动用户管理器、并在退出登录后保持运行，由有权限的用户执行：

```sh
loginctl enable-linger alice
```

安装和管理用户服务需要可连接的 systemd 用户管理器；不应以 root 的用户会话替代目标用户执行 `--scope user`。

### Linux：systemd 系统服务

系统安装必须明确提供目标用户名、用户主目录。建议同时明确 Codex 目录，以免 `sudo` 改变或清除 `CODEX_HOME`。

```sh
sudo ./target/release/codex-app-server-bridge \
  --user-home /home/alice --codex-home /home/alice/.codex \
  service install --scope system --username alice

sudo /home/alice/.codex-app-server-bridge/codex-app-server-bridge --user-home /home/alice --codex-home /home/alice/.codex service status --scope system --username alice
sudo /home/alice/.codex-app-server-bridge/codex-app-server-bridge --user-home /home/alice --codex-home /home/alice/.codex service stop --scope system --username alice
sudo /home/alice/.codex-app-server-bridge/codex-app-server-bridge --user-home /home/alice --codex-home /home/alice/.codex service start --scope system --username alice
sudo /home/alice/.codex-app-server-bridge/codex-app-server-bridge --user-home /home/alice --codex-home /home/alice/.codex service uninstall --scope system --username alice
```

服务文件为 `/etc/systemd/system/codex-app-server-bridge.service`。服务进程以 `alice` 的身份运行，需要能够访问二进制、配置快照和 Codex socket。安装会将专用安装目录、状态目录、程序副本及配置快照归属目标用户，程序副本权限为 `0755`；用户主目录内为此新建的父目录也归目标用户所有。已有父目录的所有者和权限保持不变，不递归更改用户主目录或 Codex 目录权限。用户主目录必须事先存在。

### macOS：登录启动 LaunchAgent

在目标用户已登录的桌面会话中执行：

```sh
codex-app-server-bridge service install --scope user
"$HOME/.codex-app-server-bridge/codex-app-server-bridge" service status --scope user
"$HOME/.codex-app-server-bridge/codex-app-server-bridge" service stop --scope user
"$HOME/.codex-app-server-bridge/codex-app-server-bridge" service start --scope user
"$HOME/.codex-app-server-bridge/codex-app-server-bridge" service uninstall --scope user
```

配置文件为 `~/Library/LaunchAgents/io.codex.app-server-bridge.plist`，注册到当前用户的 `gui/<uid>` 域。没有 GUI 登录会话时应选择合适的系统服务方式。

### macOS：开机启动 LaunchDaemon

```sh
sudo ./target/release/codex-app-server-bridge \
  --user-home /Users/alice --codex-home /Users/alice/.codex \
  service install --scope system --username alice

sudo /Users/alice/.codex-app-server-bridge/codex-app-server-bridge --user-home /Users/alice --codex-home /Users/alice/.codex service status --scope system --username alice
sudo /Users/alice/.codex-app-server-bridge/codex-app-server-bridge --user-home /Users/alice --codex-home /Users/alice/.codex service stop --scope system --username alice
sudo /Users/alice/.codex-app-server-bridge/codex-app-server-bridge --user-home /Users/alice --codex-home /Users/alice/.codex service start --scope system --username alice
sudo /Users/alice/.codex-app-server-bridge/codex-app-server-bridge --user-home /Users/alice --codex-home /Users/alice/.codex service uninstall --scope system --username alice
```

配置文件为 `/Library/LaunchDaemons/io.codex.app-server-bridge.plist`，通过 `UserName` 按指定用户身份运行。用户目录或 Codex socket 在登录前尚不可用时，桥接的实际可用时间仍取决于这些资源；LaunchDaemon 不能提前解锁加密的用户数据。

### 卸载与日志

`service uninstall` 停止并移除对应启动项。`~/.codex-app-server-bridge` 中的程序副本、`config.toml`（或自定义配置文件）、配置快照和历史日志保留，方便重新安装或排查。正常停止会撤回本实例发布的环境变量；强制终止或断电无法保证立即清理，下次运行会根据记录恢复清理。

| 运行方式 | 日志 |
| --- | --- |
| 前台 | 标准错误输出 |
| Windows 后台启动 / 系统服务 | `<state_dir>/logs/bridge.log` |
| Linux 用户服务 | `journalctl --user -u codex-app-server-bridge -f` |
| Linux 系统服务 | `sudo journalctl -u codex-app-server-bridge -f` |
| macOS launchd | `<state_dir>/service.stdout.log` 和 `service.stderr.log` |

`status` 检查实例锁并输出运行状态；`service status` 查询操作系统启动项或服务状态。服务处于运行状态并不意味着 Codex socket 已经就绪，仍需查看 `status` 的 `ready` 字段。

## 协议与访问范围

实现依据 OpenAI Codex 官方 `rust-v0.162.1` 源码：[`unix_socket.rs`](https://github.com/openai/codex/blob/rust-v0.162.1/codex-rs/app-server-transport/src/transport/unix_socket.rs) 在 UDS 上执行 WebSocket 握手，跨平台 UDS 实现在 [`codex-rs/uds/src/lib.rs`](https://github.com/openai/codex/blob/rust-v0.162.1/codex-rs/uds/src/lib.rs)。本桥接因此采用 **WebSocket over TCP ↔ WebSocket over UDS**，不按逐行 JSON 文本解析 socket。

每个客户端连接对应一个独立的上游连接，转发文本和二进制消息。socket 被关闭或替换时，已有会话可能断开；桥接会重新检测可用性，但客户端仍须自己重连并重新建立所需的 Codex 会话，不会自动重放请求。

只监听 loopback 地址，只接受根路径 `/`，拒绝查询参数和带 `Origin` 的握手；`Host` 必须与发布的 URL 一致。不会转发 `/daemon/shutdown` 等其他路径。浏览器页面连接会被拒绝，应使用原生 WebSocket 客户端。

桥接没有额外的令牌认证。loopback 限制远程网络访问，不能隔离同机其他用户；应运行在可信的本机环境中，并按需要通过受控隧道访问。此实现针对上述 Codex 控制 socket 协议，其他版本若更改握手或协议，需要重新验证。
