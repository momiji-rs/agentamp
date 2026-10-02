//! Turning a target into tracks the engine can queue.

use anyhow::{Result, bail};

use crate::library;
use crate::model::{Source, Track};
use crate::target::{self, Target};

pub async fn tracks(target: &str) -> Result<Vec<Track>> {
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
        Target::Spotify { .. } => bail!("Spotify is not available yet"),
        Target::Youtube(_) => bail!("YouTube is not available yet"),
    }
}
