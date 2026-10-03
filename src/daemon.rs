//! The background process that keeps playing while no window is open.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use log::{info, warn};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot};

use crate::deck::{Deck, NullDeck, RodioDeck};
use crate::engine::{Engine, Mode, Msg};
use crate::ipc::{Request, Response};
use crate::paths::Paths;
use crate::resolve;
use crate::spotify::Spotify;
use crate::tap::Tap;

pub const DEFAULT_VOLUME: u8 = 80;
/// How often a listening window is sent the sound since the last send.
const LISTEN_TICK: std::time::Duration = std::time::Duration::from_millis(8);
/// Ticks without sound between the chunks of none that check the window is there.
const HEARTBEAT: u32 = 125;

pub async fn run(paths: Paths) -> Result<()> {
    let socket = paths.socket();
    let listener = bind(&socket)?;
    info!("listening on {}", socket.display());

    // AGENTAMP_AUDIO=null plays nothing and keeps time: tests, demos, CI.
    let tap = Arc::new(Tap::default());
    let files: Box<dyn Deck> = match std::env::var("AGENTAMP_AUDIO").as_deref() {
        Ok("null") => Box::new(NullDeck::default()),
        _ => Box::new(RodioDeck::new(DEFAULT_VOLUME, tap.clone())),
    };
    let spotify = Spotify::new(paths.clone());
    let (tx, rx) = mpsc::unbounded_channel();
    let engine = tokio::spawn(Engine::new(files, DEFAULT_VOLUME, spotify.clone(), tx.clone(), tap.clone()).run(rx));

    let accept = async {
        loop {
            let (stream, _) = listener.accept().await?;
            tokio::spawn(serve(stream, tx.clone(), paths.clone(), spotify.clone(), tap.clone()));
        }
        #[allow(unreachable_code)]
        Ok::<(), std::io::Error>(())
    };
    tokio::select! {
        result = accept => result?,
        _ = engine => info!("shutting down"),
        _ = tokio::signal::ctrl_c() => info!("interrupted"),
    }
    let _ = std::fs::remove_file(&socket);
    Ok(())
}

/// Binds the socket, replacing one left behind by a daemon that died.
fn bind(socket: &Path) -> Result<UnixListener> {
    let dir = socket.parent().context("socket path has no directory")?;
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    if socket.exists() {
        if std::os::unix::net::UnixStream::connect(socket).is_ok() {
            bail!("AgentAmp is already running ({})", socket.display());
        }
        std::fs::remove_file(socket)?;
    }
    let listener = UnixListener::bind(socket)?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

async fn serve(
    stream: UnixStream,
    tx: mpsc::UnboundedSender<Msg>,
    paths: Paths,
    spotify: Arc<Spotify>,
    tap: Arc<Tap>,
) {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(Request::Listen) => return listen(write, &tap).await,
            Ok(request) => handle(request, &tx, &paths, &spotify).await,
            Err(e) => Response::error(format!("not a request: {e}")),
        };
        let mut out = serde_json::to_string(&response).unwrap_or_default();
        out.push('\n');
        if write.write_all(out.as_bytes()).await.is_err() {
            break;
        }
    }
}

/// Sends a window the sound as it plays, until the window hangs up.
async fn listen(mut write: OwnedWriteHalf, tap: &Arc<Tap>) {
    let mut out = serde_json::to_vec(&Response::ok(())).unwrap_or_default();
    out.push(b'\n');
    if write.write_all(&out).await.is_err() {
        return;
    }
    let mut listener = tap.listen();
    let mut tick = tokio::time::interval(LISTEN_TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let (mut samples, mut quiet) = (Vec::new(), 0u32);
    loop {
        tick.tick().await;
        samples.clear();
        let rate = listener.take(&mut samples);
        // Now and then a chunk of none, so a window gone is noticed.
        quiet = if samples.is_empty() { quiet + 1 } else { 0 };
        if samples.is_empty() && quiet % HEARTBEAT != 0 {
            continue;
        }
        out.clear();
        crate::tap::encode(rate, &samples, &mut out);
        if write.write_all(&out).await.is_err() {
            return;
        }
    }
}

async fn handle(
    request: Request,
    tx: &mpsc::UnboundedSender<Msg>,
    paths: &Paths,
    spotify: &Arc<Spotify>,
) -> Response {
    let (reply, answer) = oneshot::channel();
    let msg = match request {
        Request::Play { target } | Request::Add { target, .. } if target.trim().is_empty() => {
            return Response::error("nothing to play: give a link, a search or a path");
        }
        Request::Play { ref target } | Request::Add { ref target, .. } => {
            let mode = match &request {
                Request::Play { .. } => Mode::Replace,
                Request::Add { next: true, .. } => Mode::Next,
                _ => Mode::Append,
            };
            match resolve::tracks(target, paths, spotify, tx).await {
                Ok(tracks) => Msg::Enqueue { tracks, mode, reply },
                Err(e) => {
                    warn!("cannot play {target}: {e:#}");
                    return Response::error(e);
                }
            }
        }
        request => Msg::Control(request, reply),
    };
    if tx.send(msg).is_err() {
        return Response::error("the player has stopped");
    }
    answer.await.unwrap_or_else(|_| Response::error("the player has stopped"))
}
