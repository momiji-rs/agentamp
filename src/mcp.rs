//! `agentamp mcp`: the player as a Model Context Protocol server on stdio,
//! for agents that speak MCP rather than run commands. The official SDK
//! keeps to the spec; each tool passes its request to the daemon as the CLI
//! does, starting it when needed. Stdout carries only the protocol.

use std::sync::Arc;

use anyhow::Result;
use rmcp::handler::server::{router::tool::ToolRouter, wrapper::Parameters};
use rmcp::model::{Implementation, ServerCapabilities, ServerConfig};
use rmcp::{Json, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::ipc::Request;
use crate::model::{Status, Track, plain_integers};
use crate::paths::Paths;
use crate::youtube::Found;

const INSTRUCTIONS: &str = "AgentAmp plays Spotify (Premium, after `agentamp login`), YouTube and local files \
through a background player that keeps going between calls. `play` replaces the queue, `add` extends it, \
`now_playing` and `queue` say what is on without changing it. `search_youtube` lists videos to choose \
from; pass a result's `target` to `play` or `add`.";

pub fn run(paths: Paths) -> Result<()> {
    tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async {
        let service = Player::new(paths).serve(rmcp::transport::stdio()).await?;
        service.waiting().await?;
        Ok(())
    })
}

#[derive(Deserialize, JsonSchema)]
struct Target {
    /// A Spotify track, album or playlist link or `spotify:` URI, a YouTube link, `yt:` followed by a
    /// search for its first result, or the absolute path of an audio file or a folder of them.
    target: String,
}

#[derive(Deserialize, JsonSchema)]
struct Add {
    /// The same kinds of target `play` takes.
    target: String,
    /// Put it straight after the current track instead of at the end.
    #[serde(default)]
    next: bool,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(transform = plain_integers)]
struct Volume {
    /// 0 to 100.
    #[schemars(range(max = 100))]
    percent: u8,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(transform = plain_integers)]
struct Seek {
    /// Seconds from the start of the track.
    seconds: u32,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(transform = plain_integers)]
struct Search {
    /// What to look for, as you would type it into YouTube.
    query: String,
    /// How many results, 1 to 20.
    #[serde(default = "five")]
    #[schemars(range(min = 1, max = 20))]
    count: u8,
}

fn five() -> u8 {
    5
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct Results {
    /// In YouTube's order, best match first.
    results: Vec<Found>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[schemars(transform = plain_integers)]
struct Added {
    /// How many tracks the target held.
    added: usize,
    status: Status,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct Queue {
    current: Option<Track>,
    /// What plays after the current track, in order.
    upcoming: Vec<Track>,
}

#[derive(Clone)]
struct Player {
    paths: Arc<Paths>,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl Player {
    fn new(paths: Paths) -> Self {
        Self { paths: Arc::new(paths), tool_router: Self::tool_router() }
    }

    /// Asks the daemon, off the protocol's thread. Its refusals become tool
    /// errors the agent reads and can act on.
    async fn ask<T: DeserializeOwned>(&self, request: Request) -> Result<Json<T>, String> {
        let paths = self.paths.clone();
        let autostart = !matches!(request, Request::Status | Request::Queue);
        let data = tokio::task::spawn_blocking(move || crate::send(&paths, &request, autostart))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("{e:#}"))?;
        serde_json::from_value(data).map(Json).map_err(|e| format!("the player's answer is not understood: {e}"))
    }

    /// Play now, replacing the queue with everything the target holds.
    #[tool(annotations(title = "Play", destructive_hint = true, idempotent_hint = false, open_world_hint = true))]
    async fn play(&self, Parameters(Target { target }): Parameters<Target>) -> Result<Json<Added>, String> {
        self.ask(Request::Play { target: crate::here(&target) }).await
    }

    /// Add to the queue, at its end or straight after the current track. A stopped player starts.
    #[tool(annotations(title = "Add to queue", destructive_hint = false, idempotent_hint = false, open_world_hint = true))]
    async fn add(&self, Parameters(Add { target, next }): Parameters<Add>) -> Result<Json<Added>, String> {
        self.ask(Request::Add { target: crate::here(&target), next }).await
    }

    /// Pause. Pausing a paused or stopped player changes nothing.
    #[tool(annotations(title = "Pause", destructive_hint = false, idempotent_hint = true, open_world_hint = false))]
    async fn pause(&self) -> Result<Json<Status>, String> {
        self.ask(Request::Pause).await
    }

    /// Resume a paused track, or after a stop start the next one in the queue.
    #[tool(annotations(title = "Resume", destructive_hint = false, idempotent_hint = true, open_world_hint = false))]
    async fn resume(&self) -> Result<Json<Status>, String> {
        self.ask(Request::Resume).await
    }

    /// Skip to the next track in the queue.
    #[tool(annotations(title = "Next", destructive_hint = false, idempotent_hint = false, open_world_hint = false))]
    async fn next(&self) -> Result<Json<Status>, String> {
        self.ask(Request::Next).await
    }

    /// Back to the start of the track, or to the track before when this one has only just begun.
    #[tool(annotations(title = "Previous", destructive_hint = false, idempotent_hint = false, open_world_hint = false))]
    async fn previous(&self) -> Result<Json<Status>, String> {
        self.ask(Request::Previous).await
    }

    /// Stop the current track. The rest of the queue stays; `resume` starts its next track.
    #[tool(annotations(title = "Stop", destructive_hint = false, idempotent_hint = true, open_world_hint = false))]
    async fn stop(&self) -> Result<Json<Status>, String> {
        self.ask(Request::Stop).await
    }

    /// Empty the queue after the current track, which plays on.
    #[tool(annotations(title = "Clear queue", destructive_hint = true, idempotent_hint = true, open_world_hint = false))]
    async fn clear_queue(&self) -> Result<Json<Status>, String> {
        self.ask(Request::Clear).await
    }

    /// Set the volume, 0 to 100.
    #[tool(annotations(title = "Set volume", destructive_hint = false, idempotent_hint = true, open_world_hint = false))]
    async fn set_volume(&self, Parameters(Volume { percent }): Parameters<Volume>) -> Result<Json<Status>, String> {
        if percent > 100 {
            return Err(format!("the volume goes from 0 to 100, not {percent}"));
        }
        self.ask(Request::Volume { percent }).await
    }

    /// Jump to a position in the current track.
    #[tool(annotations(title = "Seek", destructive_hint = false, idempotent_hint = true, open_world_hint = false))]
    async fn seek(&self, Parameters(Seek { seconds }): Parameters<Seek>) -> Result<Json<Status>, String> {
        self.ask(Request::Seek { position_ms: seconds.saturating_mul(1000) }).await
    }

    /// Search YouTube for videos to play, without downloading or playing any. Live streams are left out.
    #[tool(annotations(title = "Search YouTube", read_only_hint = true, open_world_hint = true))]
    async fn search_youtube(&self, Parameters(Search { query, count }): Parameters<Search>) -> Result<Json<Results>, String> {
        if query.trim().is_empty() {
            return Err("say what to search for".into());
        }
        if !(1..=20).contains(&count) {
            return Err(format!("a search lists 1 to 20 results, not {count}"));
        }
        let results = crate::youtube::search(query.trim(), count).await.map_err(|e| format!("{e:#}"))?;
        Ok(Json(Results { results }))
    }

    /// What is playing, where in it, and the volume. Never starts the player.
    #[tool(annotations(title = "Now playing", read_only_hint = true, open_world_hint = false))]
    async fn now_playing(&self) -> Result<Json<Status>, String> {
        self.ask(Request::Status).await
    }

    /// The current track and what plays after it. Never starts the player.
    #[tool(annotations(title = "Queue", read_only_hint = true, open_world_hint = false))]
    async fn queue(&self) -> Result<Json<Queue>, String> {
        self.ask(Request::Queue).await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Player {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("agentamp", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }
}
