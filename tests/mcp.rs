//! `agentamp mcp` spoken to as an MCP client would, in raw JSON-RPC over
//! stdio, against a real daemon with silent audio.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use serde_json::{Value, json};

struct Server {
    home: PathBuf,
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    id: u64,
}

impl Server {
    fn start(name: &str) -> Self {
        let home = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_agentamp"))
            .arg("mcp")
            .env("AGENTAMP_HOME", &home)
            .env("AGENTAMP_AUDIO", "null")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if tx.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        Self { home, child, stdin, lines, id: 0 }
    }

    fn send(&mut self, message: Value) {
        writeln!(self.stdin, "{message}").unwrap();
        self.stdin.flush().unwrap();
    }

    /// Sends a request and waits for its response. Every line on stdout
    /// must be a JSON-RPC message.
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        let id = self.id;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let line = self.lines.recv_timeout(Duration::from_secs(30)).expect("no answer");
            let message: Value = serde_json::from_str(&line).unwrap_or_else(|_| panic!("not JSON on stdout: {line}"));
            assert_eq!(message["jsonrpc"], "2.0", "{line}");
            if message["id"] == id {
                return message;
            }
        }
    }

    /// The result of a request that must succeed.
    fn result(&mut self, method: &str, params: Value) -> Value {
        let answer = self.request(method, params);
        assert!(answer.get("error").is_none(), "{method}: {answer}");
        answer["result"].clone()
    }

    /// Calls a tool as a 2026-07-28 client, which has no handshake and
    /// says who it is on every request.
    fn call(&mut self, tool: &str, arguments: Value) -> Value {
        self.result("tools/call", json!({"name": tool, "arguments": arguments, "_meta": meta()}))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = Command::new(env!("CARGO_BIN_EXE_agentamp")).arg("quit").env("AGENTAMP_HOME", &self.home).status();
    }
}

fn meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {"name": "test", "version": "0"},
        "io.modelcontextprotocol/clientCapabilities": {},
    })
}

fn wav(path: &Path, seconds: u32) {
    let rate = 8000u32;
    let data = rate * seconds * 2;
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    for v in [1u16, 1] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    for v in [2u16, 16] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    out.resize(out.len() + data as usize, 0);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, out).unwrap();
}

#[test]
fn clients_with_the_handshake_get_every_tool() {
    let mut server = Server::start("mcp_handshake");
    let init = server.result(
        "initialize",
        json!({"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}),
    );
    assert_eq!(init["protocolVersion"], "2025-11-25");
    assert_eq!(init["serverInfo"]["name"], "agentamp");
    assert!(init["capabilities"]["tools"].is_object());
    server.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    let tools = server.result("tools/list", json!({}))["tools"].as_array().unwrap().clone();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "add", "clear_queue", "next", "now_playing", "pause", "play", "previous", "queue", "resume", "seek",
            "set_volume", "stop"
        ]
    );
    for tool in &tools {
        assert!(tool["description"].as_str().is_some_and(|d| !d.is_empty()), "{tool}");
        assert_eq!(tool["inputSchema"]["type"], "object", "{tool}");
        assert!(tool["outputSchema"].is_object(), "{tool}");
        // Unset hints default to destructive and open-world, so every tool says.
        let hints = &tool["annotations"];
        assert!(hints["readOnlyHint"] == true || hints["destructiveHint"].is_boolean(), "{tool}");
        assert!(hints["openWorldHint"].is_boolean(), "{tool}");
    }
    // schemars' `uint32`-style formats are no JSON Schema format; validators warn about them.
    assert!(!json!(tools).to_string().contains(r#""format":"uint"#), "{tools:?}");
    let tool = |name: &str| tools.iter().find(|t| t["name"] == name).unwrap();
    assert_eq!(tool("now_playing")["annotations"]["readOnlyHint"], true);
    assert_eq!(tool("play")["annotations"]["destructiveHint"], true);
    assert_eq!(tool("play")["inputSchema"]["required"], json!(["target"]));
    assert_eq!(tool("set_volume")["inputSchema"]["properties"]["percent"]["maximum"], 100);
}

#[test]
fn clients_without_the_handshake_play_and_control() {
    let mut server = Server::start("mcp_stateless");
    let discovered = server.result("server/discover", json!({"_meta": meta()}));
    assert!(discovered["supportedVersions"].as_array().unwrap().contains(&json!("2026-07-28")), "{discovered}");

    let now = server.call("now_playing", json!({}));
    assert_eq!(now["isError"], false);
    assert_eq!(now["structuredContent"]["state"], "stopped");
    assert!(!server.home.join("run/agentamp.sock").exists(), "now_playing must not start the player");

    let music = server.home.join("music");
    wav(&music.join("1 First.wav"), 60);
    wav(&music.join("2 Second.wav"), 60);
    let played = server.call("play", json!({"target": music}));
    assert_eq!(played["isError"], false, "{played}");
    let added = &played["structuredContent"];
    assert_eq!(added["added"], 2);
    assert_eq!(added["status"]["state"], "playing");
    assert_eq!(added["status"]["track"]["title"], "1 First");
    // The same JSON as text, for clients that read no structured content.
    let text: Value = serde_json::from_str(played["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(&text, added);

    let queue = server.call("queue", json!({}))["structuredContent"].clone();
    assert_eq!(queue["current"]["title"], "1 First");
    assert_eq!(queue["upcoming"][0]["title"], "2 Second");

    assert_eq!(server.call("pause", json!({}))["structuredContent"]["state"], "paused");
    assert_eq!(server.call("resume", json!({}))["structuredContent"]["state"], "playing");
    let seek = server.call("seek", json!({"seconds": 30}))["structuredContent"]["position_ms"].as_u64().unwrap();
    assert!((30_000..32_000).contains(&seek), "{seek}");
    assert_eq!(server.call("set_volume", json!({"percent": 35}))["structuredContent"]["volume"], 35);
    assert_eq!(server.call("next", json!({}))["structuredContent"]["track"]["title"], "2 Second");
    assert_eq!(server.call("clear_queue", json!({}))["structuredContent"]["queue_len"], 0);
    assert_eq!(server.call("stop", json!({}))["structuredContent"]["state"], "stopped");

    // What the agent can correct comes back as a tool error it reads.
    let loud = server.call("set_volume", json!({"percent": 150}));
    assert_eq!(loud["isError"], true);
    assert!(loud["content"][0]["text"].as_str().unwrap().contains("0 to 100"), "{loud}");
    let missing = server.call("add", json!({"target": "/no/such/song.mp3"}));
    assert_eq!(missing["isError"], true);
    assert!(missing["content"][0]["text"].as_str().unwrap().contains("no such file"), "{missing}");

    // An unknown tool is a protocol error.
    let unknown = server.request("tools/call", json!({"name": "dance", "arguments": {}, "_meta": meta()}));
    assert_eq!(unknown["error"]["code"], -32602, "{unknown}");
}
