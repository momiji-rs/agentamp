//! Copies the Spotify library into the database: the Liked Songs, with
//! when each was liked, and their albums' release dates, label and kind,
//! which a liked song does not carry. The albums come from the catalogue's
//! metadata, many to a request, as librespot lets a session make 300
//! requests in 30 s and playback needs its share.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Result, bail};
use librespot_core::Session;
use librespot_protocol::extended_metadata::{BatchedEntityRequest, EntityRequest, ExtensionQuery};
use librespot_protocol::extension_kind::ExtensionKind;
use librespot_protocol::metadata::{self, album::Type};
use log::{info, warn};
use protobuf::{EnumOrUnknown, Message};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::db::{self, Album, Liked};
use crate::model::plain_integers;
use crate::spotify::Spotify;
use crate::spotify_search::artists;

/// The most songs `fetchLibraryTracks` gives at once: 10 000 gives none
/// (2026-10-04).
const PAGE: usize = 1_000;
/// Pages under way at once.
const AT_ONCE: usize = 4;
/// Albums asked for in one request: 500 answered in 0.3 s (2026-10-04).
const ALBUMS: usize = 500;

/// What a sync found.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(transform = plain_integers)]
pub struct Synced {
    /// The Liked Songs.
    pub liked: usize,
    /// Liked since the last sync.
    pub new: usize,
    /// No longer liked.
    pub gone: usize,
    /// Albums read for the first time.
    pub albums_read: usize,
    /// Albums Spotify did not give; the next sync asks again.
    pub albums_failed: usize,
}

static SYNCING: AtomicBool = AtomicBool::new(false);

struct Syncing;

impl Drop for Syncing {
    fn drop(&mut self) {
        SYNCING.store(false, Ordering::SeqCst);
    }
}

pub async fn sync(spotify: &Arc<Spotify>, database: &Path) -> Result<Synced> {
    if SYNCING.swap(true, Ordering::SeqCst) {
        bail!("the library is already being synced");
    }
    let _syncing = Syncing;
    let first = spotify.query("fetchLibraryTracks", json!({"offset": 0, "limit": PAGE})).await?;
    let total = first["me"]["library"]["tracks"]["totalCount"].as_u64().map(|n| n as usize);
    let Some(total) = total else { bail!("Spotify did not give the Liked Songs") };
    let rest = (PAGE..total).step_by(PAGE).map(|offset| ("fetchLibraryTracks", json!({"offset": offset, "limit": PAGE})));
    let mut pages = vec![Ok(first)];
    pages.extend(each(spotify, rest.collect()).await);
    let mut songs = Vec::with_capacity(total);
    for (n, page) in pages.into_iter().enumerate() {
        let items = page?["me"]["library"]["tracks"]["items"].as_array().cloned().unwrap_or_default();
        // A page short of its share would have the sync forget songs still liked.
        if items.len() < PAGE.min(total - n * PAGE) {
            bail!("the Liked Songs changed while they were read; sync again");
        }
        songs.extend(items.iter().filter_map(liked));
    }

    let path = database.to_path_buf();
    let (liked, (new, gone), missing) = blocking(move || {
        let mut db = db::open(&path)?;
        let counts = db::replace_liked(&mut db, &songs)?;
        Ok((songs.len(), counts, db::albums_missing(&db)?))
    })
    .await?;
    info!("synced {liked} Liked Songs, {new} new, {gone} gone; reading {} albums", missing.len());

    let session = spotify.session().await?;
    let mut albums = Vec::new();
    for batch in missing.chunks(ALBUMS) {
        match read_albums(&session, batch).await {
            Ok(read) => albums.extend(read),
            Err(e) => warn!("cannot read {} albums: {e:#}", batch.len()),
        }
    }
    let albums_read = albums.len();
    let path: PathBuf = database.to_path_buf();
    blocking(move || db::insert_albums(&mut db::open(&path)?, &albums)).await?;
    Ok(Synced { liked, new, gone, albums_read, albums_failed: missing.len() - albums_read })
}

/// The albums of `uris` the catalogue gives, in one request.
async fn read_albums(session: &Session, uris: &[String]) -> Result<Vec<Album>> {
    let query = ExtensionQuery { extension_kind: EnumOrUnknown::new(ExtensionKind::ALBUM_V4), ..Default::default() };
    let entity_request = uris
        .iter()
        .map(|uri| EntityRequest { entity_uri: uri.clone(), query: vec![query.clone()], ..Default::default() })
        .collect();
    let request = BatchedEntityRequest { entity_request, ..Default::default() };
    let answer = session.spclient().get_extended_metadata(request).await?;
    let data = answer.extended_metadata.iter().flat_map(|kind| &kind.extension_data);
    Ok(data
        .filter_map(|d| album(&d.entity_uri, &metadata::Album::parse_from_bytes(&d.extension_data.value).ok()?))
        .collect())
}

/// The web player's answers to `asks`, in their order, `AT_ONCE` under way.
async fn each(spotify: &Arc<Spotify>, asks: Vec<(&'static str, Value)>) -> Vec<Result<Value>> {
    let gate = Arc::new(tokio::sync::Semaphore::new(AT_ONCE));
    let mut tasks = tokio::task::JoinSet::new();
    for (n, (op, variables)) in asks.into_iter().enumerate() {
        let (spotify, gate) = (spotify.clone(), gate.clone());
        tasks.spawn(async move {
            let _turn = gate.acquire().await;
            (n, spotify.query(op, variables).await)
        });
    }
    let mut answers = tasks.join_all().await;
    answers.sort_by_key(|(n, _)| *n);
    answers.into_iter().map(|(_, answer)| answer).collect()
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f).await?
}

/// A song of a `fetchLibraryTracks` page. Its URI is on the wrapper only.
fn liked(item: &Value) -> Option<Liked> {
    let data = &item["track"]["data"];
    Some(Liked {
        uri: item["track"]["_uri"].as_str()?.into(),
        added_at: item["addedAt"]["isoString"].as_str()?.into(),
        title: data["name"].as_str().unwrap_or_default().into(),
        artist: artists(data),
        album: data["albumOfTrack"]["name"].as_str().unwrap_or_default().into(),
        album_uri: data["albumOfTrack"]["uri"].as_str().map(str::to_string),
        duration_ms: data["duration"]["totalMilliseconds"].as_u64().unwrap_or(0) as u32,
    })
}

/// An album as the catalogue describes it, dated as precisely as it knows.
fn album(uri: &str, album: &metadata::Album) -> Option<Album> {
    if !album.has_name() {
        return None;
    }
    let date = &album.date;
    let released = date.has_year().then(|| match (date.has_month(), date.has_day()) {
        (true, true) => format!("{:04}-{:02}-{:02}", date.year(), date.month(), date.day()),
        (true, false) => format!("{:04}-{:02}", date.year(), date.month()),
        _ => format!("{:04}", date.year()),
    });
    let kind = album.has_type().then(|| match album.type_() {
        Type::ALBUM => "ALBUM",
        Type::SINGLE => "SINGLE",
        Type::COMPILATION => "COMPILATION",
        Type::EP => "EP",
        Type::AUDIOBOOK => "AUDIOBOOK",
        Type::PODCAST => "PODCAST",
    });
    let names: Vec<&str> = album.artist.iter().map(|a| a.name()).collect();
    Some(Album {
        uri: uri.into(),
        title: album.name().into(),
        artist: names.join(", "),
        released,
        label: Some(album.label()).filter(|l| !l.is_empty()).map(str::to_string),
        kind: kind.map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_liked_song_keeps_when_it_was_liked_and_its_album() {
        let item = json!({"__typename": "UserLibraryTrackResponse", "addedAt": {"isoString": "2026-10-01T19:55:02Z"},
            "track": {"_uri": "spotify:track:1", "data": {"__typename": "Track", "name": "Boston",
                "artists": {"items": [{"profile": {"name": "STELLA LEFTY"}}, {"profile": {"name": "Guest"}}]},
                "albumOfTrack": {"name": "Long Way Home", "uri": "spotify:album:0inYFsNCyffdWte267wXRW"},
                "duration": {"totalMilliseconds": 170859}, "playability": {"playable": false}}}});
        assert_eq!(liked(&item), Some(Liked {
            uri: "spotify:track:1".into(),
            added_at: "2026-10-01T19:55:02Z".into(),
            title: "Boston".into(),
            artist: "STELLA LEFTY, Guest".into(),
            album: "Long Way Home".into(),
            album_uri: Some("spotify:album:0inYFsNCyffdWte267wXRW".into()),
            duration_ms: 170_859,
        }), "a song that cannot play now is still liked");
        let bare = json!({"addedAt": {"isoString": "2026-10-01T19:55:02Z"}, "track": {"_uri": "spotify:track:2", "data": null}});
        assert_eq!(liked(&bare).map(|s| (s.uri, s.album_uri)), Some(("spotify:track:2".into(), None)));
        assert_eq!(liked(&json!({"track": {"_uri": "spotify:track:3"}})), None, "no time, no row");
    }

    #[test]
    fn an_album_is_dated_as_precisely_as_spotify_knows() {
        let mut data = metadata::Album::new();
        data.set_name("TRAD".into());
        data.set_label("Ariola".into());
        data.set_type(Type::ALBUM);
        let mut artist = metadata::Artist::new();
        artist.set_name("Mariya Takeuchi".into());
        data.artist.push(artist);
        data.date.mut_or_insert_default().set_year(2014);
        data.date.mut_or_insert_default().set_month(9);
        data.date.mut_or_insert_default().set_day(6);
        // As it comes over the wire.
        let data = metadata::Album::parse_from_bytes(&data.write_to_bytes().unwrap()).unwrap();
        assert_eq!(album("spotify:album:x", &data), Some(Album {
            uri: "spotify:album:x".into(),
            title: "TRAD".into(),
            artist: "Mariya Takeuchi".into(),
            released: Some("2014-09-06".into()),
            label: Some("Ariola".into()),
            kind: Some("ALBUM".into()),
        }));
        let mut month = data.clone();
        month.date.mut_or_insert_default().clear_day();
        assert_eq!(album("spotify:album:x", &month).unwrap().released.as_deref(), Some("2014-09"));
        let mut year = month.clone();
        year.date.mut_or_insert_default().clear_month();
        assert_eq!(album("spotify:album:x", &year).unwrap().released.as_deref(), Some("2014"));
        let mut undated = data.clone();
        undated.date.clear();
        let undated = album("spotify:album:x", &undated).unwrap();
        assert_eq!((undated.released, undated.label.is_some()), (None, true));
        assert_eq!(album("spotify:album:x", &metadata::Album::new()), None, "an entity the catalogue has nothing for");
    }
}
