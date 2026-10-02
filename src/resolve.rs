//! Turning a target into tracks the engine can queue.

use std::sync::Arc;

use anyhow::{Result, bail};
use log::warn;
use tokio::sync::mpsc;

use crate::engine::Msg;
use crate::library;
use crate::model::{Source, Track};
use crate::paths::Paths;
use crate::spotify::Spotify;
use crate::target::{self, Target};
use crate::youtube;

/// How many resolved Spotify tracks reach the queue at once.
const BATCH: usize = 10;

/// The tracks `target` names. A Spotify album or playlist returns at once
/// with its first track's details; the rest follow as `Msg::Resolved`.
pub async fn tracks(
    target: &str,
    paths: &Paths,
    spotify: &Arc<Spotify>,
    tx: &mpsc::UnboundedSender<Msg>,
) -> Result<Vec<Track>> {
    match target::parse(target)? {
        Target::File(path) => {
            Ok(vec![tokio::task::spawn_blocking(move || library::read(&path, Source::Local)).await?])
        }
        Target::Folder(folder) => {
            tokio::task::spawn_blocking(move || {
                let files = target::audio_files(&folder);
                if files.is_empty() {
                    bail!("no audio files in {}", folder.display());
                }
                Ok(files.iter().map(|p| library::read(p, Source::Local)).collect())
            })
            .await?
        }
        Target::Spotify { kind, uri } => {
            let mut tracks = spotify.expand(kind, &uri).await?;
            let Some(first) = tracks.first_mut() else {
                bail!("found no tracks there");
            };
            *first = spotify.details(&first.uri).await?;
            let rest: Vec<String> = tracks.iter().skip(1).map(|t| t.uri.clone()).collect();
            let (spotify, tx) = (spotify.clone(), tx.clone());
            tokio::spawn(async move {
                for chunk in rest.chunks(BATCH) {
                    let mut batch = Vec::with_capacity(chunk.len());
                    for uri in chunk {
                        match spotify.details(uri).await {
                            Ok(track) => batch.push(track),
                            Err(e) => warn!("no details for {uri}: {e:#}"),
                        }
                    }
                    if tx.send(Msg::Resolved(batch)).is_err() {
                        break;
                    }
                }
            });
            Ok(tracks)
        }
        Target::Youtube(url) => Ok(vec![youtube::fetch(&url, &paths.youtube_audio()).await?]),
    }
}
