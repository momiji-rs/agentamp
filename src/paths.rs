//! Where AgentAmp keeps its files.

use std::path::PathBuf;

use anyhow::{Context, Result};

#[derive(Clone, Debug)]
pub struct Paths {
    /// Spotify credentials.
    pub config: PathBuf,
    /// Spotify's encrypted audio cache and downloaded YouTube audio.
    pub cache: PathBuf,
    /// The daemon's socket.
    pub runtime: PathBuf,
}

impl Paths {
    pub fn new() -> Result<Self> {
        if let Some(root) = std::env::var_os("AGENTAMP_HOME") {
            return Ok(Self::under(&PathBuf::from(root)));
        }
        let dirs = directories::ProjectDirs::from("", "", "agentamp")
            .context("no home directory to keep AgentAmp's files in")?;
        let runtime = dirs
            .runtime_dir()
            .map(PathBuf::from)
            .unwrap_or_else(|| dirs.cache_dir().join("run"));
        Ok(Self {
            config: dirs.config_dir().to_path_buf(),
            cache: dirs.cache_dir().to_path_buf(),
            runtime,
        })
    }

    /// Every file under one directory, as `AGENTAMP_HOME` asks.
    pub fn under(root: &std::path::Path) -> Self {
        Self {
            config: root.join("config"),
            cache: root.join("cache"),
            runtime: root.join("run"),
        }
    }

    pub fn socket(&self) -> PathBuf {
        self.runtime.join("agentamp.sock")
    }

    pub fn youtube_audio(&self) -> PathBuf {
        self.cache.join("youtube")
    }

    pub fn spotify_audio(&self) -> PathBuf {
        self.cache.join("spotify-audio")
    }

    /// Downloaded covers, named after their URL.
    pub fn art(&self) -> PathBuf {
        self.cache.join("art")
    }

    pub fn log(&self) -> PathBuf {
        self.cache.join("agentamp.log")
    }
}
