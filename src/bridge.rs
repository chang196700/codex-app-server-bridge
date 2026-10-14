use crate::{
    config::{Settings, private_dir},
    environment::Publisher,
    socket,
};
use anyhow::{Context, Result, bail};
use fs2::FileExt;
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use serde::Serialize;
use std::{
    fs::OpenOptions,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    net::{TcpListener, TcpStream},
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
    socket_path: &'a std::path::Path,
    updated_at_unix: u64,
}

fn write_status(settings: &Settings, url: &str, ready: bool) -> Result<()> {
    let state = Status {
        pid: std::process::id(),
        ready,
        url,
        socket_path: &settings.socket_path,
        updated_at_unix: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
    };
    crate::write_atomic(
        &settings.state_dir.join("status.json"),
        &serde_json::to_vec_pretty(&state)?,
    )
}

fn ws_config(settings: &Settings) -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(settings.config.max_message_size))
        .max_frame_size(Some(settings.config.max_message_size))
}

async fn upstream(settings: &Settings) -> Result<WebSocketStream<socket::Stream>> {
    timeout(settings.timeout(), async {
        let stream = socket::connect(&settings.socket_path).await?;
        let (websocket, _) =
            client_async_with_config("ws://localhost/", stream, Some(ws_config(settings))).await?;
        Ok::<_, anyhow::Error>(websocket)
    })
    .await
    .context("control socket connection/handshake timed out")?
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
                        "socket unavailable",
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
    timeout(settings.timeout(), async {
        ws.close(None).await?;
        // Finish the close handshake rather than leaving resets in server logs.
        let _ = ws.next().await;
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("control socket close handshake timed out")??;
    Ok(())
}

// Registry broadcasts and session-manager commands can block. Keep publication
// serialized in a separate worker and let the listener continue accepting.
async fn environment_worker(
    settings: Arc<Settings>,
    url: String,
    mut desired: watch::Receiver<bool>,
) -> Result<()> {
    if !settings.config.publish_environment {
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
    let listener = TcpListener::bind(SocketAddr::new(settings.config.host, settings.config.port))
        .await
        .context("cannot bind WebSocket port (a fixed port is never silently changed)")?;
    let address = listener.local_addr()?;
    let url = format!("ws://{address}/");
    let ready = Arc::new(AtomicBool::new(false));
    let settings = Arc::new(settings);
    let mut tasks = JoinSet::new();
    let capacity = Arc::new(Semaphore::new(settings.config.max_connections));
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
    let health_settings = settings.clone();
    let monitor = tokio::spawn(async move {
        let mut ticker = interval(Duration::from_millis(
            health_settings.config.poll_interval_ms,
        ));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            let health = probe(&health_settings).await;
            if let Err(error) = &health {
                tracing::debug!(%error, "control socket probe failed");
            }
            if health_tx.send(health.is_ok()).is_err() {
                break;
            }
        }
    });
    tracing::info!(%url, socket = %settings.socket_path.display(), "listening; waiting for the Codex control socket");
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
                    changed.context("control socket monitor stopped unexpectedly")?;
                    let is_ready = *health_rx.borrow_and_update();
                    let was_ready = ready.swap(is_ready, Ordering::AcqRel);
                    if is_ready && !was_ready { tracing::info!(%url, "control socket ready"); }
                    if !is_ready && was_ready {
                        tracing::warn!("control socket unavailable; waiting for recovery");
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
