//! The background process that keeps playing while no window is open.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use log::{info, warn};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot};

use crate::deck::{Deck, NullDeck, RodioDeck};
use crate::engine::{Engine, Mode, Msg};
use crate::ipc::{Request, Response};
use crate::paths::Paths;
use crate::resolve;
use crate::spotify::Spotify;

const DEFAULT_VOLUME: u8 = 80;

pub async fn run(paths: Paths) -> Result<()> {
    let socket = paths.socket();
    let listener = bind(&socket)?;
    info!("listening on {}", socket.display());

    // AGENTAMP_AUDIO=null plays nothing and keeps time: tests, demos, CI.
    let files: Box<dyn Deck> = match std::env::var("AGENTAMP_AUDIO").as_deref() {
        Ok("null") => Box::new(NullDeck::default()),
        _ => Box::new(RodioDeck::new(DEFAULT_VOLUME)),
    };
    let spotify = Spotify::new(paths.clone());
    let (tx, rx) = mpsc::unbounded_channel();
    let engine = tokio::spawn(Engine::new(files, DEFAULT_VOLUME, spotify.clone(), tx.clone()).run(rx));

    let accept = async {
        loop {
            let (stream, _) = listener.accept().await?;
            tokio::spawn(serve(stream, tx.clone(), paths.clone(), spotify.clone()));
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

async fn serve(stream: UnixStream, tx: mpsc::UnboundedSender<Msg>, paths: Paths, spotify: Arc<Spotify>) {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let response = match serde_json::from_str::<Request>(&line) {
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
