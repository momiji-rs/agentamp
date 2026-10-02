//! Where AgentAmp keeps its files.

use std::path::PathBuf;

use anyhow::{Context, Result};

#[derive(Clone, Debug)]
pub struct Paths {
    /// Spotify's encrypted audio cache and downloaded YouTube audio.
    pub cache: PathBuf,
    /// The daemon's socket.
    pub runtime: PathBuf,
}

impl Paths {
    pub fn new() -> Result<Self> {
        if let Some(root) = std::env::var_os("AGENTAMP_HOME") {
            let root = PathBuf::from(root);
            return Ok(Self {
                cache: root.join("cache"),
                runtime: root.join("run"),
            });
        }
        let dirs = directories::ProjectDirs::from("", "", "agentamp")
            .context("no home directory to keep AgentAmp's files in")?;
        let runtime = dirs
            .runtime_dir()
            .map(PathBuf::from)
            .unwrap_or_else(|| dirs.cache_dir().join("run"));
        Ok(Self {
            cache: dirs.cache_dir().to_path_buf(),
            runtime,
        })
    }

    pub fn socket(&self) -> PathBuf {
        self.runtime.join("agentamp.sock")
    }

    pub fn youtube_audio(&self) -> PathBuf {
        self.cache.join("youtube")
    }

    pub fn log(&self) -> PathBuf {
        self.cache.join("agentamp.log")
    }
}
