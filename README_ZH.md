# codex-app-server-bridge

[English](README.md) | 简体中文

使用 Rust 将 Codex app-server 暴露为 `ws://127.0.0.1:<端口>/`。默认情况下，每个 WebSocket 客户端对应一个 `codex app-server proxy` 子进程，桥接通过 WebSocket 转发 proxy 的 STDIO 字节流，因此不依赖 Codex 的 control socket 路径。也可以选择 `socket` 直连传输以兼容旧方式。可选开启自动设置用户环境变量 `CODEX_APP_SERVER_WS_URL`，默认关闭。

支持 Windows、macOS 和 Linux；支持多个 WebSocket 监听、固定端口、动态端口，以及后台服务或登录启动。可选开启在 Codex app-server daemon 未运行时自动启动，默认关闭。默认 proxy 模式下由 Codex 负责发现和管理 control endpoint。

## 下载与自动发布

从 [GitHub Releases](https://github.com/chang196700/codex-app-server-bridge/releases) 下载适合操作系统与架构的压缩包。每个 Release 附带 `SHA256SUMS`，每个压缩包内包含程序、中英文 README、`LICENSE-MIT`、`LICENSE-APACHE` 和构建信息。

| 平台 | 架构 | 压缩格式 |
| --- | --- | --- |
| Linux（GNU/glibc） | x86_64、ARM64 | `.tar.gz` |
| Linux（静态 musl） | x86_64、ARM64 | `.tar.gz` |
| macOS | Intel x86_64、Apple Silicon ARM64 | `.tar.gz` |
| Windows（MSVC） | x86_64、ARM64 | `.zip` |

八种构建均使用对应架构的 GitHub 原生 runner。Linux GNU 使用 Ubuntu 24.04 构建，产物面向 glibc 2.39 及以上系统。musl 产物在相同 x86_64/ARM64 架构 runner 的 Alpine 容器中静态链接，不依赖 glibc，适用于 Alpine 或 glibc 较旧的发行版；构建时会检查产物没有动态加载器及共享库依赖。

推送到 `main` 后，工作流自动运行 [release-plz](https://release-plz.dev/) 更新版本，并根据 Git commit message 生成 `CHANGELOG.md`，由 `github-actions[bot]` 提交这些变更。自动发布提交本身不计入变更日志。若版本尚未打标签便重跑准备步骤，会重新生成该版本章节，包含期间新增的提交；已有标签的版本章节保持不变。八个目标全部通过格式检查、Clippy、release 编译和程序版本检查后，工作流创建 `v<版本>` 标签，并发布包含完整压缩包和校验文件的 GitHub Release。无需手动合并发布 PR，也不发布到 crates.io。版本号按 release-plz 的提交分析规则更新，建议使用 `fix: ...`、`feat: ...`、`feat!: ...` 等 Conventional Commits 表达变更类型。

工作流使用内置 `GITHUB_TOKEN`，无需额外配置 Secrets。`main` 必须允许工作流提交版本变更；该 Token 创建的提交不会再次触发工作流。发布串行执行，排队的任务读取最新 `main`，因此短时间内的多次推送可能合并为一个 Release。发布中断时，可以重跑失败的任务，或在 `main` 上手动运行 **Release** 工作流。已有标签始终对应原始源码，完整发布的版本不会被覆盖。构建失败时，版本提交保留在 `main`，不会发布缺少产物的 Release。

## 从 GitHub 更新

```sh
# 更新已安装的用户服务或登录启动程序
codex-app-server-bridge update

# 仅更新执行当前命令的程序文件
codex-app-server-bridge update --self

# 定位使用自定义配置文件 / 状态目录安装的服务
codex-app-server-bridge --config /path/to/bridge.toml update
```

`update` 会检查操作系统中的服务或启动项注册，更新所选用户 `~/.codex-app-server-bridge` 目录中的固定程序副本。未安装服务时会报错提示未安装；`service uninstall` 遗留的程序文件不算已安装。默认作用范围为 `user`。

命令读取已安装程序的版本，从 GitHub 下载最新稳定 Release 中匹配更新程序操作系统和架构的压缩包。Linux 下，GNU 更新程序选择 GNU/glibc 包，musl 更新程序选择 musl 包。替换文件前会根据该 Release 的 `SHA256SUMS` 校验压缩包；目标版本已等于或高于最新版本时，不修改目标文件，也不会降级。

服务更新保留启动注册和所选配置文件。运行中的服务在下载和校验完成后才会停止，并在替换程序后重启、重新读取该文件；已停止的服务保持停止。替换或重启失败时，会尝试回滚到原程序和服务状态。定位自定义服务时，使用安装时相同的 `--user-home` 和 `--config` 选择。

更新 Linux 或 macOS 系统服务时，需要管理员权限，并明确提供目标用户名和用户主目录：

```sh
# Linux；macOS 将用户主目录改为 /Users/alice
sudo /home/alice/.codex-app-server-bridge/codex-app-server-bridge \
  --user-home /home/alice update --scope system --username alice
```

更新 Windows 系统服务时，在目标用户本人提升权限的管理员 PowerShell 中执行：

```powershell
& 'C:\Users\alice\.codex-app-server-bridge\codex-app-server-bridge.exe' `
  --user-home 'C:\Users\alice' update --scope system
```

`update --self` 替换执行当前命令的程序文件，不读取配置，不要求已安装服务，也不停止或重启服务；不能与 `--scope` 或 `--username` 组合使用。执行用户需要拥有程序所在目录的写入权限。如果从固定服务程序路径执行 `--self`，该路径的文件也会被替换，但已运行的服务进程仍需重启才能使用新版本。要自动让运行中的服务应用新版本，请使用 `update`。

## 构建与运行

安装 Rust / rustup 和当前平台的 Rust 原生构建工具。仓库中的 `rust-toolchain.toml` 固定使用当前最新稳定版 Rust 1.99.0，CI 原生构建也使用同一版本。

```sh
cargo build --release --locked
```

构建产物为 `target/release/codex-app-server-bridge`；Windows 为同目录下的 `codex-app-server-bridge.exe`。以下命令假设程序已加入 `PATH`；也可以直接使用产物的绝对路径。

```sh
# 创建 ~/.codex-app-server-bridge/config.toml，已有文件不会被覆盖
codex-app-server-bridge init-config

# 前台运行；不带子命令也等同于 run
codex-app-server-bridge run

# 查看实际端口、传输方式、PID 和就绪状态
codex-app-server-bridge status
```

启动时 proxy 或 socket endpoint 尚不可用也可以：桥接会保持运行并定期尝试连接。前台使用 `Ctrl+C` 退出。

```sh
# 固定端口；端口被占用会报错退出，不会悄悄改用其他端口
codex-app-server-bridge --port 8765 run

# 动态分配一个可用端口
codex-app-server-bridge --port 0 run

# 指定 Codex 数据目录或配置文件
codex-app-server-bridge --codex-home /path/to/.codex run
codex-app-server-bridge --config /path/to/bridge.toml run
```

动态端口在一次运行期间保持不变；桥接进程重启后可能变化。只重启 Codex 或其 proxy endpoint，不会使仍在运行的桥接更换监听端口。

## 配置

默认配置为 `~/.codex-app-server-bridge/config.toml`。`.codex-app-server-bridge` 目录也用于存放为后台启动安装的程序副本。Windows 中 `~` 对应当前用户主目录。默认文件不存在时使用内置配置。读取显式 `--config` 指定的文件时，文件不存在则报错；`init-config` 和 `config set` 可以创建该文件。`init-config` 会创建目录及示例配置，不覆盖已有配置。

如果 `~/.codex-app-server-bridge` 仍是旧版 TOML 文件，选用默认配置时会自动将其内容迁移到 `~/.codex-app-server-bridge/config.toml`，并将原内容保留为 `~/.codex-app-server-bridge.legacy-<唯一标识>.toml.bak`。显式 `--config` 指向旧默认路径时也会转为使用新文件。无效 TOML 文件和符号链接不会迁移。迁移中断时会保留备份，重新执行命令可继续恢复，不覆盖内容冲突的文件。迁移保留原配置，因此随后执行 `init-config` 仍会拒绝覆盖已有文件。

```toml
# 可省略；默认使用 CODEX_HOME，未设置时使用 ~/.codex
# codex_home = "~/.codex"

# 默认通过 `codex app-server proxy` 的 STDIO 连接，因此桥接不依赖 Codex 的 control socket 路径。
# 只有需要直接兼容 control socket 时才设置为 socket。
transport = "proxy"

# 仅当 transport = "socket" 时使用；默认派生自 codex_home
# socket_path = "~/.codex/app-server-control/app-server-control.sock"

auto_start_codex_app_server_daemon = false  # daemon 未运行时自动启动，默认关闭
# 可选；省略时从 PATH 和常见安装位置自动检测 Codex
# codex_path = "/absolute/path/to/codex"

poll_interval_ms = 2000         # 上游健康检查间隔，至少 100 毫秒

# 必须使用本程序专用的状态目录
# state_dir = "~/.local/state/codex-app-server-bridge"

[websocket]
host = "127.0.0.1"             # 也可使用 ::1；只接受 loopback 地址
port = 0                       # 0 为动态端口；例如 8765 为固定端口
connect_timeout_ms = 3000       # 连接和握手超时，至少 100 毫秒
max_connections = 64
max_message_size = 67108864     # 本桥接的消息/帧上限；仍受 Codex 上游限制
publish_codex_app_server_ws_url_environment = false  # 默认关闭；设为 true 自动设置用户环境变量
# 可选鉴权；两个字段都省略时，客户端无需提供 token。
# auth_token_file 与 auth_token 只能选一个。
# auth_token_file = "~/.codex-app-server-bridge/websocket.token"
# auth_token = "replace-with-a-long-random-token"
```

默认 WebSocket 监听使用 `[websocket]` 表。升级前，如果已有配置在顶层使用 `host`、`port`、`connect_timeout_ms`、`max_connections`、`max_message_size` 或 `publish_codex_app_server_ws_url_environment`，请先将这些字段移入 `[websocket]`；新版本会拒绝旧的顶层字段，不会自动迁移。`poll_interval_ms`、传输方式、daemon 和路径配置仍放在顶层，写在 `[websocket]` 之前。

使用 `[websockets.<name>]` 添加具名监听。名称不能为空，也不能包含点号或控制字符：

```toml
[websockets.tools]
host = "127.0.0.1"
port = 63101
connect_timeout_ms = 3000
max_connections = 32
max_message_size = 67108864

[websockets.ipv6]
host = "::1"
port = 0
```

所有监听共用顶层的 Codex 传输方式、daemon 和路径配置。每个监听分别设置端口、连接数上限、消息大小上限、连接超时及可选鉴权；省略字段时使用内置默认值，不继承 `[websocket]`，鉴权配置也不继承。端口为 `0` 时，各监听独立选择动态端口。具名监听不支持 `publish_codex_app_server_ws_url_environment`，即使设为 `false` 也会报错；只有默认 `[websocket]` 可以发布 `CODEX_APP_SERVER_WS_URL`。

配置优先级：

| 项目 | 优先级，从高到低 |
| --- | --- |
| 配置文件 | `--config`；否则 `--user-home` 或操作系统当前用户主目录下的 `.codex-app-server-bridge/config.toml` |
| 用户主目录 | `--user-home` → TOML `user_home` → 操作系统当前用户主目录 |
| Codex 目录 | `--codex-home` → TOML `codex_home` → 环境变量 `CODEX_HOME` → 所选用户主目录下的 `.codex` |
| 上游传输 | TOML `transport` → `proxy`（通过 Codex `app-server proxy` 的 STDIO） |
| socket 路径 | 仅 `transport = "socket"` 时使用；TOML `socket_path` → `<codex_home>/app-server-control/app-server-control.sock` |
| Codex 执行文件 | TOML `codex_path` → 从 `PATH` 和常见安装位置自动检测 |
| 默认监听端口 | `--port` → TOML `websocket.port` → `0`；具名监听使用各自的 `websockets.<name>.port` |

TOML 中的 `user_home` 在读取配置后生效，不会改变已经选定的配置文件路径。路径开头的 `~` 会展开为所选用户主目录；其他相对路径相对于启动命令时的工作目录。安装服务时会将路径固定为绝对路径。之后修改服务使用的路径时，请使用绝对路径或 `~`，避免依赖服务管理器的工作目录。TOML 未知字段会报错，避免拼写错误被忽略。

### 查看和修改配置

```sh
codex-app-server-bridge config list
codex-app-server-bridge config get websocket.port
codex-app-server-bridge config set websocket.port 8765
codex-app-server-bridge config set websocket.publish_codex_app_server_ws_url_environment true
codex-app-server-bridge config set websockets.tools.port 63101
codex-app-server-bridge config get websockets.tools.port
codex-app-server-bridge config set websockets.tools.auth_token_file '~/.codex-app-server-bridge/tools.token'
codex-app-server-bridge config get websockets.tools.auth_token_file
codex-app-server-bridge config set transport proxy
codex-app-server-bridge config set socket_path '~/.codex/app-server-control/app-server-control.sock'
codex-app-server-bridge config get auto_start_codex_app_server_daemon
codex-app-server-bridge config get codex_path

# 可选：指定执行文件；使用自动检测时省略这条命令
codex-app-server-bridge config set codex_path /absolute/path/to/codex
codex-app-server-bridge config set auto_start_codex_app_server_daemon true

# 查看或修改自定义配置文件
codex-app-server-bridge --config /path/to/bridge.toml config get websocket.port
codex-app-server-bridge --config /path/to/bridge.toml config set websocket.port 8765
```

`config list` 输出配置文件与内置默认值合并后的 JSON 对象。`config get KEY` 仅以 JSON 输出指定字段的值：字符串带引号，未设置的可选字段为 `null`。这些命令显示配置值，不应用 `CODEX_HOME` 等运行时环境变量或 `--port` 等命令行覆盖。

`config set KEY VALUE` 接受与字段类型匹配的值。数字和布尔值使用 TOML 语法；路径、`websocket.host` 等字符串字段也可直接传入裸字符串，如上方示例。没有用于删除字段的特殊 unset 或 `null` 语法。该命令会创建缺失的配置文件，包括显式 `--config` 指定的文件；保留其他字段和注释，校验完整配置后再写入，并以 JSON 输出保存后的值，不会将运行时覆盖值写回文件。

配置键支持使用点分路径导航：`[websocket]` 内的字段使用 `websocket.port` 等路径，具名监听使用 `websockets.tools.port` 等路径，顶层字段使用 `transport`、`socket_path` 等名称。设置新名称下的字段会创建对应监听。未知字段或子键会被拒绝。

服务直接读取 `config list`、`config get`、`config set` 所选的同一份配置：默认 `~/.codex-app-server-bridge/config.toml`，或安装时通过 `--config` 指定的文件。修改配置不会使运行中的实例自动重新加载。前台进程需要重启；已安装服务执行 `service restart` 即可，操作系统重启服务或重新开机也会应用修改。启动已停止的服务同样会重新读取配置。

```sh
codex-app-server-bridge config set websocket.port 63100
codex-app-server-bridge service restart --scope user

# 配置命令与服务命令使用同一份自定义文件
codex-app-server-bridge --config /path/to/bridge.toml config set auto_start_codex_app_server_daemon true
codex-app-server-bridge --config /path/to/bridge.toml service restart --scope user
```

修改 `user_home` 或 `state_dir` 会影响安装位置或运行状态目录。应先停止服务，再修改这些字段，然后使用目标主目录和配置文件重新执行 `service install`；不要依靠简单重启迁移安装位置。

## WebSocket 监听鉴权

`[websocket]` 和 `[websockets.<name>]` 都支持可选的 `auth_token` 或 `auth_token_file`。默认省略两个字段，不启用鉴权；同一监听的两个字段互斥。具名监听不继承默认监听的 token。

推荐使用 `auth_token_file`，将 token 单独存放在 TOML 配置之外。文件只包含 token，可以带一个行尾换行，运行桥接的账号需要有读取权限。文件总大小（含换行）不能超过 64 KiB。例如，默认监听可以不设 token 并发布环境变量，同时让额外端口使用独立 token：

```toml
[websocket]
port = 63100
publish_codex_app_server_ws_url_environment = true

[websockets.tools]
port = 63101
auth_token_file = "~/.codex-app-server-bridge/tools.token"
# 也可以删去 auth_token_file，改为：
# auth_token = "replace-with-a-long-random-token"
```

桥接在启动时读取一次 token 文件，并移除末尾的 CR/LF 换行字符。文件无法读取、token 为空或格式无效时，启动会失败。token 遵循 RFC 6750 的 Bearer-token 格式：一个或多个 ASCII 字母、数字或 `-._~+/` 字符，末尾可以带 `=`，不允许内嵌空白。修改文件后需要重启桥接。token 文件路径遵循其他配置路径相同的 `~` 展开和相对路径规则。两个字段都支持 `config get` 和 `config set`，例如 `config set websockets.tools.auth_token_file '~/.codex-app-server-bridge/tools.token'`。直接配置的 `auth_token` 会显示在 `config list` 和 `config get` 输出中，使用 token 文件可避免配置输出包含 token 原文。

让原生 WebSocket 客户端连接 `ws://127.0.0.1:63101/`，并在 HTTP 握手时携带以下请求头，将示例值换成文件内的 token：

```http
Authorization: Bearer replace-with-a-long-random-token
```

这里使用 [Codex app-server 协议](https://learn.chatgpt.com/docs/app-server#protocol) 文档描述的 `Authorization: Bearer <token>` 握手请求头。桥接在接受 WebSocket 连接之前校验配置的静态 token，发生在任何 `initialize` 消息之前。缺少凭据或凭据错误时返回 HTTP `401 Unauthorized`。鉴权只发生在本地桥接，token 不转发给 Codex。当前实现校验静态 capability token，不验证签名 JWT。

token 不会写入 WebSocket URL、运行状态或发布的环境变量。`CODEX_APP_SERVER_WS_URL` 只提供 URL；连接需要鉴权的监听时，客户端还必须支持单独设置 Bearer 请求头。如果客户端只接受该环境变量，可保留默认监听不启用鉴权。所有监听仍只允许 loopback 地址。

## 自动启动 Codex daemon

设置 `auto_start_codex_app_server_daemon = true` 后，桥接会在 Codex app-server daemon 未运行时自动启动它；此选项默认关闭。开启后，桥接会在启动 daemon 前按需创建缺失的 `CODEX_HOME` 目录；关闭时不会创建该目录。需要安装支持 `codex app-server daemon start` 的 Codex CLI，该命令已在 Codex CLI 0.162.1 上检查。应使用完整 Codex CLI 安装中的执行文件；只复制单个程序文件，可能无法准备 daemon 所需的包。

桥接使用所选上游传输的健康检查，daemon 正常时保持运行。proxy 因 daemon 不可用退出，或直连 socket 返回 `NotFound` / `ConnectionRefused` 时，后台任务使用所选 `CODEX_HOME` 执行幂等的 `codex app-server daemon start` 命令。权限错误、协议不匹配和超时不会触发启动。启动任务不会阻塞 WebSocket 监听，失败会记录日志，并按 5 秒起、最长 60 秒的指数退避间隔重试。停止桥接不会停止独立运行的 Codex daemon。

省略 `codex_path` 时会从 `PATH` 和常见安装位置自动检测，也可以配置 Codex 执行文件路径；绝对路径、开头的 `~` 和相对路径遵循上文的配置路径规则。开启自动启动后，`service install` 会解析执行文件，将绝对路径写入所选配置文件，保留注释及其他设置。更改开关、`codex_path` 或 `codex_home` 后，重启桥接服务即可生效；如果 Codex 安装位置改变，应先更新 `codex_path` 再重启。

Unix 上通过 npm 安装的启动器还要求服务的 `PATH` 能找到 Node。桥接会将所选 Codex 执行文件的目录加入启动命令的 `PATH`，支持 nvm 等将 `node` 与 `codex` 放在同一目录的安装方式。如果该目录和服务原有的 `PATH` 都找不到 Node，请将 `codex_path` 指向完整 Codex 包中的原生执行文件。

默认 `transport = "proxy"` 时，自动启动不要求桥接知道 daemon 的 socket 路径，由 Codex 自己解析端点。`transport = "socket"` 保留 control socket 直连兼容性；开启自动启动时要求使用 `<codex_home>/app-server-control/app-server-control.sock` 标准路径。

Windows 自动启动 daemon 仅支持普通、未提升权限的用户进程：请在普通 PowerShell 中前台运行，或执行 `service install --scope user`。Codex 会拒绝提升权限进程和 LocalSystem 启动 daemon，因此开启此选项时安装 Windows `--scope system` 服务会明确报错。关闭此选项后，Windows 系统服务仍可桥接已经运行的 daemon。Linux 和 macOS 的系统服务以指定的 `--username` 身份运行，可以使用自动启动功能。

## 环境变量如何生效

自动设置环境变量**默认关闭**，省略 `websocket.publish_codex_app_server_ws_url_environment` 也保持关闭。只有默认 `[websocket]` 可以发布其 URL，具名 `[websockets.<name>]` 不发布环境变量。此时 WebSocket 桥接正常运行，可通过 `status` 查看 URL，程序不会修改用户环境变量或 shell 配置。

需要自动设置 `CODEX_APP_SERVER_WS_URL` 时，在 `~/.codex-app-server-bridge/config.toml` 中开启：

```toml
[websocket]
publish_codex_app_server_ws_url_environment = true
```

如果已有 `[websocket]`，请在该表中添加或修改字段，不要重复声明表。

修改后重启前台进程，已安装服务执行 `service restart`。要关闭已开启的发布功能，先停止原进程，让它清理自己发布的变量，再设为 `false` 并启动桥接或服务。

以下发布行为仅在开启时生效：桥接连接所选上游并完成 WebSocket 健康检查后才发布 URL。上游失联时撤回自己发布的变量，恢复后重新发布；正常退出时也会清理。清理会核对原值，保留用户或其他程序后来写入的不同值。

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

**已有终端和应用的环境不会被外部进程直接改变。** 已经启动的客户端需要重新加载环境或重启；上游离线后，旧进程中继承的变量也不会自动消失。Unix 的 `environment.sh` 只有在 URL 发布期间存在，离线时不要将旧 shell 变量当作实时状态；可运行 `status` 确认。

Windows 原生服务运行在 Session 0，其环境变更广播无法刷新另一会话中的 Explorer。注册表仍会更新；当前会话需要主动读取变量，或重新登录后继承。macOS 系统服务虽按目标用户身份运行，其 launchd 域仍可能限制 GUI 环境更新；这类失败会记录日志，shell 环境文件仍可使用。Linux 用户管理器尚未启动等情况也会记录日志，并在后续健康检查中重试会话环境发布。

## 安装服务或开机启动

`service install` 会将当前程序复制到所选用户主目录下的 `~/.codex-app-server-bridge/codex-app-server-bridge`（Windows 为 `codex-app-server-bridge.exe`），并将这个稳定路径注册为启动程序。可以直接从构建目录或下载目录安装，安装后可删除来源文件；服务或启动项使用期间需保留安装目录中的副本。使用 [`update`](#从-github-更新) 可从 GitHub 升级服务并保留配置文件；也可以使用新版程序再次执行相同的 `service install` 命令，更新程序副本及启动注册并重启实例。

安装目录由所选 `user_home` 决定，不随 `--config` 的位置改变。安装不会自动将目录加入 `PATH`，后续可使用安装副本的完整路径执行命令。从安装副本本身执行 `service install` 时会跳过自复制，更新配置及启动注册；安装副本可以通过 `update` 获取新版本。

`service install` 会注册并立即启动；重复安装会更新注册并重新启动。启动注册直接指向所选配置文件。安装会将必要的用户身份、Codex 目录、命令行覆盖值及解析后的 Codex 执行文件路径保存到同一份配置中，保留注释及无关设置。proxy 模式无论是否开启 daemon 自动启动，都会解析并保存 `codex_path`，以便在服务的最小环境中启动 Codex。已配置的路径会转为绝对路径以供服务使用；未指定的 `socket_path` 和 `state_dir` 继续按默认规则派生，不会固化为显式值。端口、daemon 选项等普通配置修改后，执行 `service restart` 即可生效，无需重新安装。不同启动方式共用同一状态目录的实例锁，不应同时安装用户和系统两种启动方式。

选定的注册方式还会保存到 `~/.codex-app-server-bridge/service.toml`，其中记录 `type`、`autostart`、身份、程序、配置文件、Codex 主目录和状态目录。省略后端参数执行 `service status`、`start`、`stop`、`restart`、`uninstall` 或 `update` 时会复用该文件；桥接运行配置仍保存在 `config.toml`。`--type` 可选 `user`、`system`、`scheduled-task`、`launch-agent`、`launch-daemon`，`--autostart` 可选 `login` 或 `startup`。

Linux 生成的 systemd unit 停止时仅终止桥接进程，因此停止或重启桥接服务会保留独立运行的 Codex daemon。

### Windows：当前用户登录启动

在目标用户的普通 PowerShell 中执行，不需要管理员权限。此方式写入当前用户的 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`，下次登录后启动隐藏的后台进程。

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

Run 启动命令超过 260 个 UTF-16 字符时安装会明确失败；请使用更短的配置文件路径，或选择系统服务。`stop` 会请求正常退出并等待实例锁释放，不使用强制结束进程。

### Windows：原生系统服务

在**目标用户本人提升权限的管理员 PowerShell**中执行，避免使用另一个管理员账户安装。服务注册为当前用户身份运行；Windows 可能要求该账户的登录凭据。此方式在系统启动时运行。

```powershell
$bridge = 'C:\Tools\codex-app-server-bridge.exe'
& $bridge --user-home 'C:\Users\alice' --codex-home 'C:\Users\alice\.codex' service install --type system --autostart startup
$bridge = 'C:\Users\alice\.codex-app-server-bridge\codex-app-server-bridge.exe'
& $bridge service status --scope system
& $bridge service stop --scope system
& $bridge service start --scope system
& $bridge service restart --scope system
& $bridge service uninstall --scope system
```

将示例路径替换为真实值。服务名为 `codex-app-server-bridge`，异常退出后由 SCM 尝试重新启动。Windows 的 `--username` 不适用。

### Windows：计划任务

需要按当前用户身份注册、又不想安装 SCM 系统服务时，可使用计划任务；它支持登录和系统启动两种触发方式：

```powershell
& $bridge service install --type scheduled-task --autostart login
# 或：--autostart startup
```

启动触发器使用无密码批处理令牌；如果 Windows 拒绝注册，请为当前账户授予“作为批处理作业登录”权限，或改用登录触发器。

启动触发器使用当前账户的无密码批处理令牌。如果 Windows 拒绝该任务，请为账户授予“作为批处理作业登录”权限，或改用登录时启动。

### Linux：systemd 用户服务

以目标用户登录后执行：

```sh
codex-app-server-bridge service install --scope user
"$HOME/.codex-app-server-bridge/codex-app-server-bridge" service status --scope user
"$HOME/.codex-app-server-bridge/codex-app-server-bridge" service stop --scope user
"$HOME/.codex-app-server-bridge/codex-app-server-bridge" service start --scope user
"$HOME/.codex-app-server-bridge/codex-app-server-bridge" service restart --scope user
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
sudo /home/alice/.codex-app-server-bridge/codex-app-server-bridge --user-home /home/alice service restart --scope system --username alice
sudo /home/alice/.codex-app-server-bridge/codex-app-server-bridge --user-home /home/alice --codex-home /home/alice/.codex service uninstall --scope system --username alice
```

服务文件为 `/etc/systemd/system/codex-app-server-bridge.service`。服务进程以 `alice` 的身份运行，需要能够访问二进制、所选配置文件和 Codex 主目录。proxy 模式通过 STDIO 启动 Codex，不需要固定的 socket 路径。安装会将专用安装目录、状态目录、程序副本及所选配置文件归属目标用户，程序副本权限为 `0755`；用户主目录内为此新建的父目录也归目标用户所有。已有父目录的所有者和权限保持不变，不递归更改用户主目录或 Codex 目录权限。用户主目录必须事先存在。

### macOS：登录启动 LaunchAgent

在目标用户已登录的桌面会话中执行：

```sh
codex-app-server-bridge service install --scope user
"$HOME/.codex-app-server-bridge/codex-app-server-bridge" service status --scope user
"$HOME/.codex-app-server-bridge/codex-app-server-bridge" service stop --scope user
"$HOME/.codex-app-server-bridge/codex-app-server-bridge" service start --scope user
"$HOME/.codex-app-server-bridge/codex-app-server-bridge" service restart --scope user
"$HOME/.codex-app-server-bridge/codex-app-server-bridge" service uninstall --scope user
```

配置文件为 `~/Library/LaunchAgents/io.codex.app-server-bridge.plist`，注册到当前用户的 `gui/<uid>` 域。没有 GUI 登录会话时应选择合适的系统服务方式。

### macOS：开机启动 LaunchDaemon

```sh
sudo ./target/release/codex-app-server-bridge \
  --user-home /Users/alice --codex-home /Users/alice/.codex \
  service install --type launch-daemon --autostart startup

sudo /Users/alice/.codex-app-server-bridge/codex-app-server-bridge --user-home /Users/alice --codex-home /Users/alice/.codex service status --scope system --username alice
sudo /Users/alice/.codex-app-server-bridge/codex-app-server-bridge --user-home /Users/alice --codex-home /Users/alice/.codex service stop --scope system --username alice
sudo /Users/alice/.codex-app-server-bridge/codex-app-server-bridge --user-home /Users/alice --codex-home /Users/alice/.codex service start --scope system --username alice
sudo /Users/alice/.codex-app-server-bridge/codex-app-server-bridge --user-home /Users/alice service restart --scope system --username alice
sudo /Users/alice/.codex-app-server-bridge/codex-app-server-bridge --user-home /Users/alice --codex-home /Users/alice/.codex service uninstall --scope system --username alice
```

配置文件为 `/Library/LaunchDaemons/io.codex.app-server-bridge.plist`，通过 `UserName` 默认按当前用户身份运行。用户目录或 Codex 主目录在登录前尚不可用时，桥接的实际可用时间仍取决于这些资源；LaunchDaemon 不能提前解锁加密的用户数据。

### 卸载与日志

`service uninstall` 停止并移除对应启动项。`~/.codex-app-server-bridge` 中的程序副本、`config.toml`（或自定义配置文件）和历史日志保留，方便重新安装或排查。正常停止会撤回本实例发布的环境变量；强制终止或断电无法保证立即清理，下次运行会根据记录恢复清理。

| 运行方式 | 日志 |
| --- | --- |
| 前台 | 标准错误输出 |
| Windows 后台启动 / 系统服务 | `<state_dir>/logs/bridge.log` |
| Linux 用户服务 | `journalctl --user -u codex-app-server-bridge -f` |
| Linux 系统服务 | `sudo journalctl -u codex-app-server-bridge -f` |
| macOS launchd | `<state_dir>/service.stdout.log` 和 `service.stderr.log` |

`status` 检查实例锁并输出运行状态（包括传输方式）。顶层 `url` 和 `ready` 对应默认监听，`websockets.<name>.url` 和 `websockets.<name>.ready` 对应各具名监听；URL 中包含实际分配的端口，包括动态端口。`service status` 查询操作系统启动项或服务状态。服务处于运行状态并不意味着 Codex 上游已经就绪，仍需查看 `status` 中对应监听的 `ready` 字段。

## 协议与访问范围

默认实现依据 Codex 的 `app-server proxy`：[`stdio-to-uds`](https://github.com/openai/codex/tree/rust-v0.162.1/codex-rs/stdio-to-uds) 在 proxy STDIO 与 Codex 控制端点之间转发原始字节，[`unix_socket.rs`](https://github.com/openai/codex/blob/rust-v0.162.1/codex-rs/app-server-transport/src/transport/unix_socket.rs) 负责 daemon 侧 WebSocket 协议。因此默认采用 **WebSocket over TCP ↔ WebSocket over proxy STDIO**，不依赖 socket 文件名。设置 `transport = "socket"` 时，使用兼容路径 **WebSocket over TCP ↔ WebSocket over UDS**。两种模式都不会将上游解析为逐行 JSON 文本。

每个客户端连接对应一个独立的上游连接；proxy 模式会为该连接启动并回收一个 `codex app-server proxy` 子进程，转发文本和二进制消息。上游进程或端点关闭时，已有会话可能断开；桥接会重新检测可用性，但客户端仍须自己重连并重新建立所需的 Codex 会话，不会自动重放请求。

只监听 loopback 地址，只接受根路径 `/`，拒绝查询参数和带 `Origin` 的握手；`Host` 必须与发布的 URL 一致。不会转发 `/daemon/shutdown` 等其他路径。浏览器页面连接会被拒绝，应使用原生 WebSocket 客户端。

每个监听都可以启用上文的可选 [Bearer token 鉴权](#websocket-监听鉴权)。loopback 限制远程网络访问，不能隔离同机其他用户；应运行在可信的本机环境中，并按需要通过受控隧道访问。此实现针对 Codex app-server 通过 proxy 或直连端点暴露的 WebSocket 协议，其他版本若更改握手或协议，需要重新验证。

## 许可证

采用双许可证，可选择遵循 [MIT License](LICENSE-MIT) 或 [Apache License 2.0](LICENSE-APACHE)（`MIT OR Apache-2.0`）。
