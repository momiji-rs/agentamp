//! Spotify's web player queries (pathfinder GraphQL), asked with the
//! session's sign-in. Not a public API: the web player sends no query text,
//! only the hash of a query its server keeps, and the hashes change when the
//! web player's code does. A refused hash is looked up again in the web
//! player's current code, at most once an hour, and the answer kept in the
//! cache directory.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use http_body_util::{BodyExt, Empty};
use librespot_core::Session;
use log::{info, warn};
use serde_json::{Value, json};
use tokio::sync::Mutex;

const ENDPOINT: &str = "https://api-partner.spotify.com/pathfinder/v2/query";
const WEB_PLAYER: &str = "https://open.spotify.com/";
const CODE: &str = "https://open.spotifycdn.com/cdn/build/web-player/";
/// A desktop browser's: anything else is sent the mobile web player, whose
/// code has none of these queries.
const BROWSER: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";
const TIMEOUT: Duration = Duration::from_secs(15);
const LOOK_AGAIN: Duration = Duration::from_secs(3600);

/// The queries AgentAmp asks, each with its hash as the web player carried
/// it on 2026-10-03, and the part of its code that has it: the main bundle,
/// or the chunk of one of its pages.
const QUERIES: &[(&str, &str, Option<&str>)] = &[
    ("searchDesktop", "eef7cc54888d91bdd6802623477873caa3948ae173a0c34fd86827b267e94c03", Some("xpui-routes-search")),
    ("queryArtistOverview", "9f8134ef565e78621f1e1793555bd6633c5ac144ae0f89604ed3ae3f80b3c8e6", None),
    ("queryArtistDiscographyAll", "5e07d323febb57b4a56a42abbf781490e58764aa45feb6e3dc0591564fc56599", Some("xpui-routes-artist")),
    ("getAlbum", "6a74b456cd1735c9193d9e8ec8cc5184cad7ce13572210315229db3975964361", None),
    ("fetchPlaylist", "8964e8eafb21aa992a7d951d256d83285c04be2105d209262901de70cb97584a", None),
    ("libraryV3", "390c78e5b951029bad359785e69b07b536a509c581cbcd0aded5e5067f187455", None),
    ("fetchLibraryTracks", "087278b20b743578a6262c2b0b4bcd20d879c503cc359a2285baf083ef944240", None),
    ("userTopContent", "49ee15704de4a7fdeac65a02db20604aa11e46f02e809c55d9a89f6db9754356", Some("xpui-routes-profile")),
];

pub struct Pathfinder {
    /// The hashes last read from the web player's code.
    file: PathBuf,
    hashes: Mutex<Hashes>,
}

struct Hashes {
    known: HashMap<String, String>,
    looked: Option<Instant>,
}

impl Pathfinder {
    pub fn new(file: PathBuf) -> Self {
        let mut known: HashMap<String, String> = QUERIES.iter().map(|(op, hash, _)| (op.to_string(), hash.to_string())).collect();
        let kept: HashMap<String, String> =
            std::fs::read(&file).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        known.extend(kept.into_iter().filter(|(op, hash)| QUERIES.iter().any(|q| q.0 == op) && is_hash(hash)));
        Self { file, hashes: Mutex::new(Hashes { known, looked: None }) }
    }

    /// The `data` of query `op`'s answer.
    pub async fn query(&self, session: &Session, op: &str, variables: Value) -> Result<Value> {
        let hash = self.hashes.lock().await.known.get(op).cloned().with_context(|| format!("no query called {op}"))?;
        match ask(session, op, &hash, &variables).await {
            Err(e) if refused(&e) => {
                let Some(fresh) = self.look_again(op, &hash).await else { return Err(e) };
                ask(session, op, &fresh, &variables).await
            }
            answer => answer,
        }
    }

    /// A newer hash for `op` than `refused`, from the web player's code.
    async fn look_again(&self, op: &str, refused: &str) -> Option<String> {
        let mut hashes = self.hashes.lock().await;
        // Another query may have looked while this one waited.
        if let Some(hash) = hashes.known.get(op).filter(|h| *h != refused) {
            return Some(hash.clone());
        }
        if hashes.looked.is_some_and(|at| at.elapsed() < LOOK_AGAIN) {
            return None;
        }
        hashes.looked = Some(Instant::now());
        let found = match tokio::time::timeout(TIMEOUT * 2, read_code()).await {
            Ok(Ok(found)) => found,
            Ok(Err(e)) => {
                warn!("cannot read the web player's queries: {e:#}");
                return None;
            }
            Err(_) => {
                warn!("the web player's code took too long to read");
                return None;
            }
        };
        info!("read {} of the web player's queries from its code", found.len());
        hashes.known.extend(found);
        if let Err(e) = keep(&self.file, &hashes.known) {
            warn!("cannot keep the web player's queries: {e:#}");
        }
        hashes.known.get(op).filter(|h| *h != refused).cloned()
    }
}

async fn ask(session: &Session, op: &str, hash: &str, variables: &Value) -> Result<Value> {
    let body = json!({
        "operationName": op,
        "variables": variables,
        "extensions": {"persistedQuery": {"version": 1, "sha256Hash": hash}},
    });
    let answer = tokio::time::timeout(TIMEOUT, async {
        let token = session.login5().auth_token().await?;
        let client_token = session.spclient().client_token().await?;
        let request = http::Request::post(ENDPOINT)
            .header("Authorization", format!("Bearer {}", token.access_token))
            .header("client-token", client_token)
            .header("app-platform", "WebPlayer")
            .header("Content-Type", "application/json")
            .body(Bytes::from(body.to_string()))?;
        anyhow::Ok(session.http_client().request_body(request).await?)
    })
    .await
    .context("Spotify did not answer")??;
    data(serde_json::from_slice(&answer).context("Spotify's answer is not JSON")?)
}

/// An answer's `data`, unless it came without because of an error.
fn data(mut answer: Value) -> Result<Value> {
    match answer.get_mut("data").map(Value::take) {
        Some(data) if data.is_object() => Ok(data),
        _ => match answer["errors"].as_array().and_then(|e| e.first()) {
            Some(error) => bail!("{}", error["message"].as_str().unwrap_or("Spotify refused the query")),
            None => bail!("Spotify's answer has no data"),
        },
    }
}

/// Spotify no longer keeps the query under this hash.
fn refused(e: &anyhow::Error) -> bool {
    let message = format!("{e:#}");
    message.contains("412") || message.contains("PersistedQueryNotFound")
}

fn is_hash(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Every query of `QUERIES` the web player's current code has, with its hash.
async fn read_code() -> Result<HashMap<String, String>> {
    let client = hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new()).build(
        hyper_rustls::HttpsConnectorBuilder::new().with_native_roots()?.https_only().enable_http1().build(),
    );
    let get = async |url: String| -> Result<String> {
        let request = http::Request::get(&url).header("User-Agent", BROWSER).body(Empty::<Bytes>::new())?;
        let response = client.request(request).await?;
        if !response.status().is_success() {
            bail!("{url} answered {}", response.status());
        }
        Ok(String::from_utf8_lossy(&response.into_body().collect().await?.to_bytes()).into_owned())
    };
    let page = get(WEB_PLAYER.to_string()).await?;
    let main = bundle(&page).context("the web player's page names no code")?;
    let main = get(format!("{CODE}{main}")).await?;
    let mut found = hashes(&main, None);
    let mut chunks: Vec<&str> = QUERIES.iter().filter_map(|(_, _, chunk)| *chunk).collect();
    chunks.sort();
    chunks.dedup();
    for name in chunks {
        match chunk(&main, name) {
            Some(file) => found.extend(hashes(&get(format!("{CODE}{file}")).await?, Some(name))),
            None => warn!("the web player's code has no {name}"),
        }
    }
    Ok(found)
}

/// The main bundle's file, `web-player.<hex>.js`, as the page loads it.
fn bundle(page: &str) -> Option<String> {
    page.match_indices("/web-player/web-player.").find_map(|(at, marker)| {
        let rest = &page[at + marker.len()..];
        let hex = &rest[..rest.find(|c: char| !c.is_ascii_hexdigit())?];
        (!hex.is_empty() && rest[hex.len()..].starts_with(".js")).then(|| format!("web-player.{hex}.js"))
    })
}

/// The hashes in `code` of the queries `QUERIES` expects in the chunk
/// `name`, or in the main bundle for none.
fn hashes(code: &str, name: Option<&str>) -> HashMap<String, String> {
    let mut found = HashMap::new();
    for (op, _, _) in QUERIES.iter().filter(|(_, _, chunk)| *chunk == name) {
        let marker = format!("\"{op}\",\"query\",\"");
        let Some(at) = code.find(&marker) else { continue };
        let hash = code[at + marker.len()..].get(..64).unwrap_or_default();
        if is_hash(hash) {
            found.insert(op.to_string(), hash.to_string());
        }
    }
    found
}

/// The file of the chunk called `name`. The main bundle names each chunk's
/// file from two tables by chunk id, its name and its content hash:
/// `({1328:"xpui-pip-mini-player",…}[e]||e)+"."+({1049:"affed27f",…})[e]+".js"`.
fn chunk(main: &str, name: &str) -> Option<String> {
    let at = main.find(&format!(":\"{name}\""))?;
    let before = &main[..at];
    let id_start = before.rfind(|c: char| !c.is_ascii_digit())? + 1;
    let id = &before[id_start..];
    let hashes_at = at + main[at..].find("+\".\"+({")? + "+\".\"+(".len();
    let table = &main[hashes_at..hashes_at + main[hashes_at..].find('}')?];
    let entry = table.split([',', '{']).find_map(|e| e.strip_prefix(id)?.strip_prefix(":\""))?;
    let hash = entry.strip_suffix('"')?;
    (!id.is_empty() && hash.bytes().all(|b| b.is_ascii_hexdigit())).then(|| format!("{name}.{hash}.js"))
}

fn keep(file: &Path, known: &HashMap<String, String>) -> Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let partial = file.with_extension("partial");
    std::fs::write(&partial, serde_json::to_vec_pretty(known)?)?;
    std::fs::rename(&partial, file)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEARCH: &str = "eef7cc54888d91bdd6802623477873caa3948ae173a0c34fd86827b267e94c03";

    #[test]
    fn the_page_names_its_bundle() {
        let page = r#"<link href="https://open.spotifycdn.com/cdn/build/web-player/web-player.46a308ba.css" rel="stylesheet">
            <script src="https://open.spotifycdn.com/cdn/build/web-player/vendor~web-player.67984718.js"></script>
            <script src="https://open.spotifycdn.com/cdn/build/web-player/web-player.06a1e8e8.js"></script>"#;
        assert_eq!(bundle(page).as_deref(), Some("web-player.06a1e8e8.js"));
        let mobile = r#"<script src="https://open.spotifycdn.com/cdn/build/mobile-web-player/mobile-web-player.648c8564.js">"#;
        assert_eq!(bundle(mobile), None);
    }

    /// Trimmed from the web player's code as it was on 2026-10-03.
    const MAIN: &str = concat!(
        r#"let u=new l.l("queryArtistOverview","query","9f8134ef565e78621f1e1793555bd6633c5ac144ae0f89604ed3ae3f80b3c8e6",null);"#,
        r#"fo=new _.l("fetchPlaylist","query","8964e8eafb21aa992a7d951d256d83285c04be2105d209262901de70cb97584a",null),"#,
        r#"pn=new _.l("libraryV3","query","shortened",null),"#,
        r#"u.u=e=>""+(({1328:"xpui-pip-mini-player",1401:"xpui-routes-see-all-playlist-leavebehinds",5021:"xpui-routes-search",9908:"dwp-panel-section"})[e]||e)+"."+"#,
        r#"({1049:"affed27f",1328:"5f7e4751",5021:"0197b7d2",998:"b105441b"})[e]+".js",u.miniCssF=e=>""+(({1328:"xpui-pip-mini-player"})[e]||e)+".css""#,
    );

    #[test]
    fn hashes_are_read_from_the_code_that_carries_them() {
        let main = hashes(MAIN, None);
        assert_eq!(main["queryArtistOverview"], "9f8134ef565e78621f1e1793555bd6633c5ac144ae0f89604ed3ae3f80b3c8e6");
        assert_eq!(main["fetchPlaylist"], "8964e8eafb21aa992a7d951d256d83285c04be2105d209262901de70cb97584a");
        assert!(!main.contains_key("libraryV3"), "not a hash");
        assert!(!main.contains_key("searchDesktop"), "searched for in its chunk only");
        let chunk = format!(r#"sn=new _.l("searchDesktop","query","{SEARCH}",null);"#);
        assert_eq!(hashes(&chunk, Some("xpui-routes-search"))["searchDesktop"], SEARCH);
    }

    #[test]
    fn a_chunk_is_found_by_its_name() {
        assert_eq!(chunk(MAIN, "xpui-routes-search").as_deref(), Some("xpui-routes-search.0197b7d2.js"));
        assert_eq!(chunk(MAIN, "xpui-pip-mini-player").as_deref(), Some("xpui-pip-mini-player.5f7e4751.js"));
        assert_eq!(chunk(MAIN, "xpui-routes-artist"), None);
        assert_eq!(chunk(MAIN, "dwp-panel-section"), None, "a chunk without a file");
    }

    #[test]
    fn answers_give_their_data_or_their_error() {
        assert_eq!(data(json!({"data": {"a": 1}, "errors": [{"message": "part"}]})).unwrap(), json!({"a": 1}));
        let refusal = data(json!({"errors": [{"message": "PersistedQueryNotFound"}]})).unwrap_err();
        assert_eq!(refusal.to_string(), "PersistedQueryNotFound");
        assert!(refused(&refusal));
        assert!(refused(&anyhow::anyhow!("Response status code: 412 Precondition Failed")));
        assert!(!refused(&anyhow::anyhow!("Response status code: 400 Bad Request")));
        assert!(data(json!({"data": null})).is_err());
    }

    #[test]
    fn kept_hashes_replace_the_built_in_ones() {
        let file = crate::testutil::scratch("pathfinder").join("queries.json");
        let newer = "0".repeat(64);
        keep(&file, &HashMap::from([
            ("searchDesktop".to_string(), newer.clone()),
            ("unknownQuery".to_string(), newer.clone()),
            ("libraryV3".to_string(), "not a hash".to_string()),
        ]))
        .unwrap();
        let known = Pathfinder::new(file).hashes.into_inner().known;
        assert_eq!(known["searchDesktop"], newer);
        assert!(!known.contains_key("unknownQuery"));
        assert_eq!(known["libraryV3"], QUERIES.iter().find(|q| q.0 == "libraryV3").unwrap().1);
    }
}
