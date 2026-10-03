//! Spotify search through the signed-in session, as Spotify's own clients
//! search: no registered Web API app. The Web API's search answers 429 for
//! librespot's client id, and a registered app could no longer stream.
//!
//! First Spotify's web player query (`searchDesktop`), which finds tracks,
//! albums, playlists and artists. It is not a public API, so when it fails
//! the session's search context still finds tracks.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use librespot_core::Session;
use log::warn;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::model::plain_integers;
use crate::spotify::Spotify;

const TIMEOUT: Duration = Duration::from_secs(15);
/// The most of each kind one search lists.
pub const MOST: u8 = 10;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Found {
    /// Best match first.
    pub tracks: Vec<Hit>,
    pub albums: Vec<Hit>,
    pub playlists: Vec<Hit>,
    pub artists: Vec<Hit>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(transform = plain_integers)]
pub struct Hit {
    /// The track's, album's or playlist's name, or the artist's.
    pub title: String,
    /// The artists, or who made the playlist. None for an artist.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub artist: String,
    /// A track's album.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    /// An album's release year.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<u16>,
    /// A track's length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u32>,
    /// The `spotify:` URI, for `play` or `add`.
    pub target: String,
}

/// Up to `count` tracks, albums, playlists and artists Spotify finds for `query`.
pub async fn search(spotify: &Arc<Spotify>, query: &str, count: u8) -> Result<Found> {
    let count = count.clamp(1, MOST);
    // The web player's own variables, but for the count.
    let variables = json!({
        "searchTerm": query, "offset": 0, "limit": count, "numberOfTopResults": 5,
        "includeAudiobooks": true, "includeArtistHasConcertsField": false, "includePreReleases": true,
        "includeAlbumPreReleases": false, "includeAuthors": false, "includeEpisodeContentRatingsV2": true,
        "isPrefix": null, "sectionFilters": ["GENERIC"],
    });
    match spotify.query("searchDesktop", variables).await.and_then(|data| parse(&data, count)) {
        Ok(found) => return Ok(found),
        Err(e) => warn!("Spotify's web search failed, searching tracks only: {e:#}"),
    }
    let session = spotify.session().await?;
    let tracks = tokio::time::timeout(TIMEOUT, context(spotify, &session, query, count))
        .await
        .context("Spotify did not answer the search")??;
    Ok(Found { tracks, ..Found::default() })
}

/// The web player's answer, keeping what can be played.
fn parse(data: &Value, count: u8) -> Result<Found> {
    let found = &data["searchV2"];
    if !found.is_object() {
        bail!("the search's answer has no results");
    }
    let items = |kind: &str| found[kind]["items"].as_array().cloned().unwrap_or_default();
    let take = |hits: Vec<Hit>| hits.into_iter().take(usize::from(count)).collect();
    Ok(Found {
        tracks: take(items("tracksV2").iter().filter_map(|i| track(&i["item"]["data"])).collect()),
        albums: take(items("albumsV2").iter().filter_map(|i| album(&i["data"])).collect()),
        playlists: take(items("playlists").iter().filter_map(|i| playlist(&i["data"])).collect()),
        artists: take(items("artists").iter().filter_map(|i| artist(&i["data"])).collect()),
    })
}

pub fn artists(data: &Value) -> String {
    let names = data["artists"]["items"].as_array().into_iter().flatten();
    names.filter_map(|a| a["profile"]["name"].as_str()).collect::<Vec<_>>().join(", ")
}

pub fn playable(data: &Value) -> bool {
    data["playability"]["playable"].as_bool() != Some(false)
}

fn hit(data: &Value, kind: &str, artist: String) -> Option<Hit> {
    let target = data["uri"].as_str().filter(|u| u.starts_with(kind))?;
    Some(Hit { title: data["name"].as_str()?.to_string(), artist, ..Hit::new(target) })
}

/// A track as the web player's queries give one, unless it cannot play.
pub fn track(data: &Value) -> Option<Hit> {
    let mut hit = hit(data, "spotify:track:", artists(data)).filter(|_| playable(data))?;
    hit.album = data["albumOfTrack"]["name"].as_str().map(str::to_string);
    let length = data["duration"]["totalMilliseconds"].as_u64().or(data["trackDuration"]["totalMilliseconds"].as_u64());
    hit.duration_ms = length.map(|ms| ms as u32);
    Some(hit)
}

pub fn album(data: &Value) -> Option<Hit> {
    let mut hit = hit(data, "spotify:album:", artists(data)).filter(|_| playable(data))?;
    let iso = data["date"]["isoString"].as_str().and_then(|d| d.get(..4)?.parse().ok());
    hit.year = data["date"]["year"].as_u64().map(|y| y as u16).or(iso);
    Some(hit)
}

pub fn playlist(data: &Value) -> Option<Hit> {
    let owner = data["ownerV2"]["data"]["name"].as_str().unwrap_or_default().to_string();
    hit(data, "spotify:playlist:", owner)
}

pub fn artist(data: &Value) -> Option<Hit> {
    let target = data["uri"].as_str().filter(|u| u.starts_with("spotify:artist:"))?;
    Some(Hit { title: data["profile"]["name"].as_str()?.to_string(), ..Hit::new(target) })
}

impl Hit {
    pub fn new(target: &str) -> Self {
        Hit { title: String::new(), artist: String::new(), album: None, year: None, duration_ms: None, target: target.to_string() }
    }
}

/// Tracks only, from the session's search context, with their details
/// read one request each.
async fn context(spotify: &Arc<Spotify>, session: &Session, query: &str, count: u8) -> Result<Vec<Hit>> {
    let context = session.spclient().get_context(&format!("spotify:search:{}", encode(query))).await?;
    let uris: Vec<String> = context
        .pages
        .iter()
        .flat_map(|p| &p.tracks)
        .filter_map(|t| t.uri.clone())
        .filter(|u| u.starts_with("spotify:track:"))
        .take(usize::from(count))
        .collect();
    let mut reads = tokio::task::JoinSet::new();
    for (i, uri) in uris.into_iter().enumerate() {
        let spotify = spotify.clone();
        reads.spawn(async move { (i, spotify.details(&uri).await) });
    }
    let mut hits = Vec::new();
    while let Some(read) = reads.join_next().await {
        match read? {
            (i, Ok(t)) => hits.push((i, Hit {
                title: t.title,
                artist: t.artist,
                album: Some(t.album).filter(|a| !a.is_empty()),
                year: None,
                duration_ms: Some(t.duration_ms),
                target: t.uri,
            })),
            (_, Err(e)) => warn!("a search result has no details: {e:#}"),
        }
    }
    hits.sort_by_key(|(i, _)| *i);
    Ok(hits.into_iter().map(|(_, hit)| hit).collect())
}

/// A search as the context URI spells it: `+` for spaces, the rest of
/// anything but letters, digits and `-_.` percent-encoded.
fn encode(query: &str) -> String {
    let mut out = String::new();
    for byte in query.trim().bytes() {
        match byte {
            b' ' => out.push('+'),
            b if b.is_ascii_alphanumeric() || b"-_.".contains(&b) => out.push(b as char),
            b => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trimmed answer of the web player's search, as it came on 2026-10-03.
    fn answer() -> Value {
        json!({"searchV2": {
            "tracksV2": {"totalCount": 800, "items": [
                {"item": {"__typename": "TrackResponseWrapper", "data": {"__typename": "Track",
                    "albumOfTrack": {"name": "Expressions", "uri": "spotify:album:3lBX7AtzE4JoZaAIBLptRx"},
                    "artists": {"items": [{"profile": {"name": "Mariya Takeuchi"}, "uri": "spotify:artist:3WwGRA2o4Ux1RRMYaYDh7N"}]},
                    "duration": {"totalMilliseconds": 294493}, "name": "Plastic Love",
                    "playability": {"playable": true, "reason": "PLAYABLE"}, "uri": "spotify:track:7rU6Iebxzlvqy5t857bKFq"}}},
                {"item": {"__typename": "TrackResponseWrapper", "data": {"__typename": "Track",
                    "albumOfTrack": {"name": "Gone", "uri": "spotify:album:0000000000000000000000"},
                    "artists": {"items": [{"profile": {"name": "Somebody"}}, {"profile": {"name": "Else"}}]},
                    "duration": {"totalMilliseconds": 1000}, "name": "Not Here",
                    "playability": {"playable": false, "reason": "NOT_AVAILABLE"}, "uri": "spotify:track:0000000000000000000000"}}},
                {"item": {"__typename": "TrackResponseWrapper", "data": {"__typename": "Track",
                    "albumOfTrack": {"name": "Plastic Love", "uri": "spotify:album:0r4h34q8ZTo1LvtXb1fIG6"},
                    "artists": {"items": [{"profile": {"name": "Friday Night Plans"}}, {"profile": {"name": "Tokyo"}}]},
                    "duration": {"totalMilliseconds": 290000}, "name": "Plastic Love",
                    "playability": {"playable": true}, "uri": "spotify:track:1111111111111111111111"}}}
            ]},
            "albumsV2": {"totalCount": 200, "items": [
                {"__typename": "AlbumResponseWrapper", "data": {"__typename": "Album",
                    "artists": {"items": [{"profile": {"name": "Friday Night Plans"}}]}, "date": {"year": 2018},
                    "name": "Plastic Love", "playability": {"playable": true}, "type": "SINGLE",
                    "uri": "spotify:album:0r4h34q8ZTo1LvtXb1fIG6"}}
            ]},
            "playlists": {"totalCount": 900, "items": [
                {"__typename": "PlaylistResponseWrapper", "data": {"__typename": "Playlist",
                    "description": "thanks for all the follows", "name": "80/90s Japanese City Pop",
                    "ownerV2": {"data": {"__typename": "User", "name": "mert.uslu13 on Instagram"}},
                    "uri": "spotify:playlist:0nz3cRJG7ZdCzdWQmIPp56"}},
                {"__typename": "NotFound", "data": {}}
            ]},
            "artists": {"totalCount": 50, "items": [
                {"__typename": "ArtistResponseWrapper", "data": {"__typename": "Artist",
                    "profile": {"name": "Miki Matsubara"}, "uri": "spotify:artist:4hUmsYcvD8C5zuVSP93jb1"}}
            ]}
        }})
    }

    #[test]
    fn the_web_search_lists_what_plays() {
        let found = parse(&answer(), 5).unwrap();
        assert_eq!(found.tracks.len(), 2, "the unplayable track is left out");
        assert_eq!(found.tracks[0], Hit {
            title: "Plastic Love".into(),
            artist: "Mariya Takeuchi".into(),
            album: Some("Expressions".into()),
            year: None,
            duration_ms: Some(294493),
            target: "spotify:track:7rU6Iebxzlvqy5t857bKFq".into(),
        });
        assert_eq!(found.tracks[1].artist, "Friday Night Plans, Tokyo");
        assert_eq!(found.albums[0].year, Some(2018));
        assert_eq!(found.albums[0].target, "spotify:album:0r4h34q8ZTo1LvtXb1fIG6");
        assert_eq!(found.playlists.len(), 1);
        assert_eq!(found.playlists[0].artist, "mert.uslu13 on Instagram");
        assert_eq!(found.artists, vec![Hit { title: "Miki Matsubara".into(), ..Hit::new("spotify:artist:4hUmsYcvD8C5zuVSP93jb1") }]);
        assert_eq!(parse(&answer(), 1).unwrap().tracks.len(), 1);
    }

    #[test]
    fn an_answer_without_results_is_an_error() {
        assert!(parse(&json!({}), 5).is_err());
    }

    #[test]
    fn the_context_search_spells_spaces_as_plus() {
        assert_eq!(encode(" plastic love "), "plastic+love");
        assert_eq!(encode("竹内"), "%E7%AB%B9%E5%86%85");
        assert_eq!(encode("AC/DC & co"), "AC%2FDC+%26+co");
    }
}
