use crate::{
    codex_path,
    config::{Settings, Transport, private_dir},
    environment::Publisher,
    socket,
};
use anyhow::{Context, Result, bail};
use fs2::FileExt;
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use serde::Serialize;
use std::{
    fs::OpenOptions,
    io,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{Semaphore, watch},
    task::JoinSet,
    time::{MissedTickBehavior, interval, timeout},
};
use tokio_tungstenite::{
    WebSocketStream, accept_hdr_async_with_config, client_async_with_config,
    tungstenite::{
        Message,
        handshake::server::{ErrorResponse, Request, Response},
        http::StatusCode,
        protocol::WebSocketConfig,
    },
};

#[derive(Serialize)]
struct Status<'a> {
    pid: u32,
    ready: bool,
    url: &'a str,
    transport: Transport,
    codex_path: Option<&'a std::path::Path>,
    socket_path: Option<&'a std::path::Path>,
    updated_at_unix: u64,
}

fn write_status(settings: &Settings, url: &str, ready: bool) -> Result<()> {
    let state = Status {
        pid: std::process::id(),
        ready,
        url,
        transport: settings.config.transport,
        codex_path: settings.config.codex_path.as_deref(),
        socket_path: settings.socket_path.as_deref(),
        updated_at_unix: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
    };
    crate::write_atomic(
        &settings.state_dir.join("status.json"),
        &serde_json::to_vec_pretty(&state)?,
    )
}

fn ws_config(settings: &Settings) -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(settings.config.websocket.max_message_size))
        .max_frame_size(Some(settings.config.websocket.max_message_size))
}

/// A bidirectional async stream backed by one Codex `app-server proxy`
/// process. The proxy speaks WebSocket frames on stdin/stdout and resolves
/// the daemon endpoint itself, so the bridge does not depend on a socket path.
struct ProxyStream {
    reader: ChildStdout,
    writer: ChildStdin,
    child: Option<Child>,
}

trait IoStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T> IoStream for T where T: AsyncRead + AsyncWrite + Unpin + Send {}

type UpstreamStream = Box<dyn IoStream>;

impl ProxyStream {
    fn new(reader: ChildStdout, writer: ChildStdin, child: Child) -> Self {
        Self {
            reader,
            writer,
            child: Some(child),
        }
    }
}

impl AsyncRead for ProxyStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        let filled = buf.filled().len();
        match std::pin::Pin::new(&mut self.reader).poll_read(cx, buf) {
            // A proxy that exits before completing the handshake has no
            // WebSocket close frame to report. Surface this as an explicit
            // unexpected EOF so daemon supervision can classify it as absent.
            std::task::Poll::Ready(Ok(())) if buf.filled().len() == filled => {
                std::task::Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Codex app-server proxy closed stdout",
                )))
            }
            result => result,
        }
    }
}

impl AsyncWrite for ProxyStream {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        data: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        std::pin::Pin::new(&mut self.writer).poll_write(cx, data)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.writer).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.writer).poll_shutdown(cx)
    }
}

impl Drop for ProxyStream {
    fn drop(&mut self) {
        // Tokio's start_kill is deliberately non-waiting and can leave a Unix
        // zombie. Move the handle into a short reaper task so every proxy is
        // both terminated and waited for when a downstream disconnects.
        let Some(mut child) = self.child.take() else {
            return;
        };
        let _ = child.start_kill();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = child.wait().await;
            });
        }
    }
}

async fn proxy_stream(settings: &Settings) -> Result<ProxyStream> {
    let executable =
        codex_path::resolve(settings.config.codex_path.as_deref(), &settings.user_home)?;
    let mut command = Command::new(&executable);
    // npm and version-manager launchers commonly require their containing
    // directory to remain in PATH. Services otherwise provide a minimal PATH.
    if let Some(directory) = executable.parent() {
        let mut paths = vec![directory.to_path_buf()];
        if let Some(path) = std::env::var_os("PATH") {
            paths.extend(std::env::split_paths(&path));
        }
        command.env(
            "PATH",
            std::env::join_paths(paths).context("prepare Codex proxy PATH")?,
        );
    }
    command
        .args(["app-server", "proxy"])
        .env("CODEX_HOME", &settings.codex_home)
        .env("HOME", &settings.user_home)
        // The bridge owns publication of this value. Do not let a proxy
        // inherit it and accidentally connect back through this listener.
        .env_remove("CODEX_APP_SERVER_WS_URL")
        .current_dir(&settings.user_home)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        // Keep proxy diagnostics in the bridge's stderr (service managers
        // route it to the bridge log) instead of allowing a stderr pipe to
        // fill and stall the proxy.
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    let mut child = command
        .spawn()
        .with_context(|| format!("launch {} app-server proxy", executable.display()))?;
    let writer = child
        .stdin
        .take()
        .context("Codex proxy did not provide stdin")?;
    let reader = child
        .stdout
        .take()
        .context("Codex proxy did not provide stdout")?;
    Ok(ProxyStream::new(reader, writer, child))
}

async fn upstream(settings: &Settings) -> Result<WebSocketStream<UpstreamStream>> {
    timeout(settings.timeout(), async {
        let stream: UpstreamStream = match settings.config.transport {
            Transport::Proxy => Box::new(proxy_stream(settings).await?),
            Transport::Socket => {
                let socket_path = settings
                    .socket_path
                    .as_deref()
                    .context("socket transport has no resolved socket_path")?;
                Box::new(socket::connect(socket_path).await?)
            }
        };
        let (websocket, _) =
            client_async_with_config("ws://localhost/", stream, Some(ws_config(settings))).await?;
        Ok::<_, anyhow::Error>(websocket)
    })
    .await
    .context("Codex upstream connection/handshake timed out")?
}

fn reject(status: StatusCode, reason: &str) -> ErrorResponse {
    let mut response = ErrorResponse::new(Some(reason.to_owned()));
    *response.status_mut() = status;
    response
}

// Tungstenite's callback API fixes the error type to an HTTP response.
#[allow(clippy::result_large_err)]
async fn connection(
    stream: TcpStream,
    settings: Arc<Settings>,
    ready: Arc<AtomicBool>,
    authority: String,
) -> Result<()> {
    // Connect upstream before accepting the downstream handshake so an accepted
    // connection always has a corresponding Codex session.
    let upstream = upstream(&settings).await?;
    let downstream = timeout(
        settings.timeout(),
        accept_hdr_async_with_config(
            stream,
            move |request: &Request, response: Response| {
                if !ready.load(Ordering::Acquire) {
                    return Err(reject(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Codex upstream unavailable",
                    ));
                }
                if request.uri().path() != "/" || request.uri().query().is_some() {
                    return Err(reject(StatusCode::NOT_FOUND, "only / is exposed"));
                }
                // Browsers must not be able to drive a local authenticated Codex
                // session from an unrelated website. Native clients omit Origin.
                if request.headers().contains_key("origin") {
                    return Err(reject(
                        StatusCode::FORBIDDEN,
                        "browser origins are not permitted",
                    ));
                }
                if request.headers().get("host").and_then(|h| h.to_str().ok())
                    != Some(authority.as_str())
                {
                    return Err(reject(
                        StatusCode::FORBIDDEN,
                        "use the published WebSocket URL",
                    ));
                }
                Ok(response)
            },
            Some(ws_config(&settings)),
        ),
    )
    .await
    .context("WebSocket handshake timed out")??;
    let (mut upstream_write, mut upstream_read) = upstream.split();
    let (mut downstream_write, mut downstream_read) = downstream.split();
    // Independent pumps prevent a slow receiver from blocking reads in the
    // opposite direction. Tungstenite answers Ping on its next read/flush.
    let result = tokio::select! {
        result = relay(&mut downstream_read, &mut upstream_write) => result,
        result = relay(&mut upstream_read, &mut downstream_write) => result,
    };
    let _ = timeout(settings.timeout(), async {
        let _ = tokio::join!(upstream_write.close(), downstream_write.close());
    })
    .await;
    result
}

async fn relay<R, W>(read: &mut R, write: &mut W) -> Result<()>
where
    R: Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
    W: Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    while let Some(message) = read.next().await {
        let message = message?;
        let closing = matches!(message, Message::Close(_));
        if matches!(
            message,
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_)
        ) {
            continue;
        }
        timeout(Duration::from_secs(30), write.send(message))
            .await
            .context("WebSocket receiver stalled for 30 seconds")??;
        if closing {
            break;
        }
    }
    Ok(())
}

async fn probe(settings: &Settings) -> Result<()> {
    let mut ws = upstream(settings).await?;
    // The successful opening handshake establishes readiness. Some Codex
    // proxies leave stdout open after a close frame, so connection cleanup
    // must not turn a reachable daemon into an unhealthy upstream.
    let cleanup = timeout(settings.timeout(), async {
        ws.close(None).await?;
        // Allow a graceful close where supported, then drop/reap the proxy.
        let _ = ws.next().await;
        Ok::<_, anyhow::Error>(())
    })
    .await;
    match cleanup {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::debug!(%error, "upstream probe cleanup failed after successful handshake")
        }
        Err(_) => tracing::debug!("upstream probe cleanup timed out after successful handshake"),
    }
    Ok(())
}

// Registry broadcasts and session-manager commands can block. Keep publication
// serialized in a separate worker and let the listener continue accepting.
async fn environment_worker(
    settings: Arc<Settings>,
    url: String,
    mut desired: watch::Receiver<bool>,
) -> Result<()> {
    if !settings
        .config
        .websocket
        .publish_codex_app_server_ws_url_environment
    {
        return Ok(());
    }
    let mut publisher = Publisher::new(
        settings.user_home.clone(),
        settings.state_dir.clone(),
        settings.config.windows_user_sid.clone(),
    );
    let mut published = false;
    loop {
        let ready = *desired.borrow_and_update();
        let current_url = url.clone();
        (publisher, published) = tokio::task::spawn_blocking(move || {
            let result = if ready {
                if published {
                    publisher.refresh_session();
                    Ok(())
                } else {
                    publisher.publish(&current_url)
                }
            } else {
                publisher.clear()
            };
            match result {
                Ok(()) => published = ready,
                Err(error) => tracing::warn!(%error, "environment update failed; will retry"),
            }
            (publisher, published)
        })
        .await
        .context("environment worker panicked")?;
        if desired.changed().await.is_err() {
            break;
        }
    }
    tokio::task::spawn_blocking(move || publisher.clear())
        .await
        .context("environment cleanup worker panicked")?
}

pub async fn run(settings: Settings, mut shutdown: watch::Receiver<bool>) -> Result<()> {
    private_dir(&settings.state_dir)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(settings.state_dir.join("bridge.lock"))?;
    FileExt::try_lock_exclusive(&lock)
        .context("another bridge is already running for this state_dir")?;
    let stop_file = settings.state_dir.join("stop.request");
    match std::fs::remove_file(&stop_file) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("cannot clear previous stop request"),
    }
    let listener = TcpListener::bind(SocketAddr::new(
        settings.config.websocket.host,
        settings.config.websocket.port,
    ))
    .await
    .context("cannot bind WebSocket port (a fixed port is never silently changed)")?;
    let address = listener.local_addr()?;
    let url = format!("ws://{address}/");
    let ready = Arc::new(AtomicBool::new(false));
    let settings = Arc::new(settings);
    let mut tasks = JoinSet::new();
    let capacity = Arc::new(Semaphore::new(settings.config.websocket.max_connections));
    let mut timer = interval(Duration::from_millis(250));
    timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
    write_status(&settings, &url, false)?;
    let (environment_tx, environment_rx) = watch::channel(false);
    let environment_task = tokio::spawn(environment_worker(
        settings.clone(),
        url.clone(),
        environment_rx,
    ));
    let (health_tx, mut health_rx) = watch::channel(false);
    let (daemon_tx, daemon_rx) = watch::channel(crate::daemon::State::Unavailable);
    let daemon_task = tokio::spawn(crate::daemon::run(settings.clone(), daemon_rx));
    let health_settings = settings.clone();
    let monitor = tokio::spawn(async move {
        let mut ticker = interval(Duration::from_millis(
            health_settings.config.poll_interval_ms,
        ));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            let health = probe(&health_settings).await;
            daemon_tx.send_replace(crate::daemon::State::from_probe(&health));
            if let Err(error) = &health {
                tracing::debug!(%error, "Codex upstream probe failed");
            }
            if health_tx.send(health.is_ok()).is_err() {
                break;
            }
        }
    });
    let transport_name = match settings.config.transport {
        Transport::Proxy => "proxy",
        Transport::Socket => "socket",
    };
    tracing::info!(
        %url,
        transport = transport_name,
        codex = ?settings.config.codex_path,
        "listening; waiting for Codex upstream"
    );
    let result: Result<()> = async {
        loop {
            if *shutdown.borrow() { break; }
            tokio::select! {
                biased;
                _ = shutdown.changed() => break,
                _ = timer.tick() => {
                    if std::fs::read_to_string(&stop_file).ok().is_some_and(|pid| pid.trim() == std::process::id().to_string()) {
                        let _ = std::fs::remove_file(&stop_file);
                        break;
                    }
                },
                changed = health_rx.changed() => {
                    changed.context("Codex upstream monitor stopped unexpectedly")?;
                    let is_ready = *health_rx.borrow_and_update();
                    let was_ready = ready.swap(is_ready, Ordering::AcqRel);
                    if is_ready && !was_ready { tracing::info!(%url, "Codex upstream ready"); }
                    if !is_ready && was_ready {
                        tracing::warn!("Codex upstream unavailable; waiting for recovery");
                        tasks.abort_all();
                    }
                    environment_tx.send_replace(is_ready);
                    write_status(&settings, &url, is_ready)?;
                },
                accepted = listener.accept() => {
                    let (stream, peer) = accepted.context("TCP accept failed")?;
                    if !ready.load(Ordering::Acquire) { continue; }
                    let Ok(permit) = capacity.clone().try_acquire_owned() else { continue; };
                    let settings = settings.clone();
                    let ready = ready.clone();
                    let authority = address.to_string();
                    tasks.spawn(async move {
                        let _permit = permit;
                        if let Err(error) = connection(stream, settings, ready, authority).await {
                            tracing::debug!(%peer, %error, "connection ended");
                        }
                    });
                },
                Some(result) = tasks.join_next(), if !tasks.is_empty() => {
                    if let Err(error) = result && !error.is_cancelled() { tracing::warn!(%error, "connection task failed"); }
                }
            }
        }
        Ok(())
    }.await;
    monitor.abort();
    let _ = monitor.await;
    daemon_task.abort();
    let _ = daemon_task.await;
    ready.store(false, Ordering::Release);
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    drop(environment_tx);
    let cleanup = environment_task
        .await
        .context("environment task panicked")
        .and_then(|result| result);
    let state_result = write_status(&settings, &url, false);
    result?;
    cleanup?;
    state_result?;
    tracing::info!("bridge stopped");
    Ok(())
}

pub fn show_status(settings: &Settings) -> Result<()> {
    let text = std::fs::read_to_string(settings.state_dir.join("status.json"))
        .context("no runtime status; start the bridge first")?;
    let mut state: serde_json::Value = serde_json::from_str(&text)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(settings.state_dir.join("bridge.lock"))?;
    let running = FileExt::try_lock_exclusive(&lock).is_err();
    state["running"] = running.into();
    if !running {
        state["ready"] = false.into();
    }
    println!("{}", serde_json::to_string_pretty(&state)?);
    if !running {
        bail!("bridge is not running (status is from its previous run)");
    }
    Ok(())
}
