//! Browsing Spotify with the web player's queries: an artist's page, an
//! album's or a playlist's tracks, the account's library and its top
//! artists and tracks. Each item carries the target to play or browse next.

use std::sync::Arc;

use anyhow::{Result, bail};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::model::plain_integers;
use crate::spotify::Spotify;
use crate::spotify_search::{Hit, album, artist, playlist, track};
use crate::target::{self, SpotifyKind, Target};

/// The most items one page lists of its paged section.
pub const MOST: u8 = 50;
/// What the library's lists may be narrowed to, as the web player names them.
const SHELVES: &[(&str, &str, &str)] =
    &[("playlists", "Playlists", "Your playlists"), ("albums", "Albums", "Your albums"), ("artists", "Artists", "Your artists")];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Page {
    pub title: String,
    /// Who made it: an album's artists, a playlist's owner.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub by: String,
    pub sections: Vec<Section>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(transform = plain_integers)]
pub struct Section {
    pub name: String,
    /// How many there are in all, which can be more than are listed.
    pub total: u32,
    pub items: Vec<Hit>,
}

impl Section {
    fn new(name: &str, total: Option<u64>, items: Vec<Hit>) -> Self {
        let total = total.map_or(items.len() as u32, |t| t as u32);
        Self { name: name.into(), total, items }
    }
}

#[derive(Debug, PartialEq)]
enum Shelf {
    Artist(String),
    Album(String),
    Playlist(String),
    Folder(String),
    /// The library narrowed to one kind: its filter and its title.
    Library(&'static str, &'static str),
    Liked,
    Top,
}

/// What `input` names to browse.
fn shelf(input: &str) -> Result<Shelf> {
    let input = input.trim();
    let word = input.to_ascii_lowercase();
    if let Some((_, filter, title)) = SHELVES.iter().find(|(name, _, _)| *name == word) {
        return Ok(Shelf::Library(filter, title));
    }
    match word.as_str() {
        "liked" => return Ok(Shelf::Liked),
        "top" => return Ok(Shelf::Top),
        _ => {}
    }
    // A folder of playlists: spotify:user:{name}:folder:{hex}
    let parts: Vec<&str> = input.split(':').collect();
    if let ["spotify", "user", _, "folder", id] = parts[..] {
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("not a Spotify folder: {input}");
        }
        return Ok(Shelf::Folder(input.to_string()));
    }
    if !input.starts_with("spotify:") && !input.starts_with("http") {
        bail!("browse takes a Spotify artist, album, playlist or folder, or playlists, albums, artists, liked or top");
    }
    match target::parse(input)? {
        Target::Spotify { kind: SpotifyKind::Artist, uri } => Ok(Shelf::Artist(uri)),
        Target::Spotify { kind: SpotifyKind::Album, uri } => Ok(Shelf::Album(uri)),
        Target::Spotify { kind: SpotifyKind::Playlist, uri } => Ok(Shelf::Playlist(uri)),
        Target::Spotify { kind: SpotifyKind::Liked, .. } => Ok(Shelf::Liked),
        Target::Spotify { kind: SpotifyKind::Track, .. } => bail!("a track has nothing to browse: play it"),
        _ => bail!("only Spotify can be browsed"),
    }
}

/// The page `input` names. `offset` and `count` page through an artist's
/// releases, an album's or playlist's tracks, the library's lists, Liked
/// Songs, and the top artists and tracks.
pub async fn browse(spotify: &Arc<Spotify>, input: &str, offset: u32, count: u8) -> Result<Page> {
    let count = count.clamp(1, MOST);
    // Each query's variables as the web player sends them, but for paging.
    match shelf(input)? {
        Shelf::Artist(uri) => {
            let (overview, releases) = tokio::join!(
                spotify.query("queryArtistOverview", json!({"uri": uri, "locale": "", "preReleaseV2": false})),
                spotify.query(
                    "queryArtistDiscographyAll",
                    json!({"uri": uri, "offset": offset, "limit": count, "order": "DATE_DESC"})
                ),
            );
            artist_page(&overview?, &releases?)
        }
        Shelf::Album(uri) => {
            album_page(&spotify.query("getAlbum", json!({"uri": uri, "locale": "", "offset": offset, "limit": count})).await?)
        }
        Shelf::Playlist(uri) => {
            let variables = json!({
                "uri": uri, "offset": offset, "limit": count,
                "enableWatchFeedEntrypoint": false, "includeEpisodeContentRatingsV2": true,
            });
            playlist_page(&spotify.query("fetchPlaylist", variables).await?)
        }
        Shelf::Folder(uri) => {
            library_page(&spotify.query("libraryV3", library("Playlists", Some(&uri), offset, count)).await?, "Playlists", "")
        }
        Shelf::Library(filter, title) => {
            library_page(&spotify.query("libraryV3", library(filter, None, offset, count)).await?, filter, title)
        }
        Shelf::Liked => liked_page(&spotify.query("fetchLibraryTracks", json!({"offset": offset, "limit": count})).await?),
        Shelf::Top => {
            let input = json!({"offset": offset, "limit": count, "sortBy": "AFFINITY", "timeRange": "SHORT_TERM"});
            let variables = json!({
                "includeTopArtists": true, "topArtistsInput": input,
                "includeTopTracks": true, "topTracksInput": input,
            });
            top_page(&spotify.query("userTopContent", variables).await?)
        }
    }
}

fn library(filter: &str, folder: Option<&str>, offset: u32, count: u8) -> Value {
    json!({
        "filters": [filter], "order": null, "textFilter": null,
        "features": ["LIKED_SONGS", "YOUR_EPISODES_V2", "CLIPS", "EVENTS"],
        "limit": count, "offset": offset, "flatten": false, "expandedFolders": [],
        "folderUri": folder, "includeFoldersWhenFlattening": true,
    })
}

fn list(value: &Value) -> impl Iterator<Item = &Value> {
    value["items"].as_array().into_iter().flatten()
}

fn found(data: &Value, kind: &str, what: &str) -> Result<()> {
    if data["__typename"].as_str() != Some(kind) {
        bail!("Spotify has no such {what}");
    }
    Ok(())
}

fn artist_page(overview: &Value, releases: &Value) -> Result<Page> {
    let page = &overview["artistUnion"];
    found(page, "Artist", "artist")?;
    let all = &releases["artistUnion"]["discography"]["all"];
    let related = &page["relatedContent"]["relatedArtists"];
    let playlists = &page["profile"]["playlistsV2"];
    let popular = list(&page["discography"]["topTracks"]).filter_map(|i| track(&i["track"]));
    let sections = [
        Section::new("Popular", None, popular.collect()),
        Section::new("Releases", all["totalCount"].as_u64(), list(all).filter_map(|i| album(&i["releases"]["items"][0])).collect()),
        Section::new("Playlists", playlists["totalCount"].as_u64(), list(playlists).filter_map(|i| playlist(&i["data"])).collect()),
        Section::new("Fans also like", related["totalCount"].as_u64(), list(related).filter_map(artist).collect()),
    ];
    Ok(Page {
        title: page["profile"]["name"].as_str().unwrap_or_default().into(),
        by: String::new(),
        // Releases page on; the others say all they have once.
        sections: sections.into_iter().filter(|s| s.name == "Releases" || !s.items.is_empty()).collect(),
    })
}

fn album_page(data: &Value) -> Result<Page> {
    let page = &data["albumUnion"];
    found(page, "Album", "album")?;
    let tracks = &page["tracksV2"];
    // An album's tracks are not given its cover again.
    let cover = crate::spotify_search::art(page);
    let items = list(tracks).filter_map(|i| track(&i["track"])).map(|t| Hit { art: t.art.clone().or(cover.clone()), ..t });
    Ok(Page {
        title: page["name"].as_str().unwrap_or_default().into(),
        by: crate::spotify_search::artists(page),
        sections: vec![Section::new("Tracks", tracks["totalCount"].as_u64(), items.collect())],
    })
}

fn playlist_page(data: &Value) -> Result<Page> {
    let page = &data["playlistV2"];
    found(page, "Playlist", "playlist")?;
    let content = &page["content"];
    let songs = list(content).map(|i| &i["itemV2"]).filter(|i| i["__typename"] == "TrackResponseWrapper");
    let tracks = songs.filter_map(|i| track(&i["data"])).collect();
    Ok(Page {
        title: page["name"].as_str().unwrap_or_default().into(),
        by: page["ownerV2"]["data"]["name"].as_str().unwrap_or_default().into(),
        sections: vec![Section::new("Tracks", content["totalCount"].as_u64(), tracks)],
    })
}

/// The library's playlists, albums or artists, or a folder's playlists.
fn library_page(data: &Value, filter: &str, title: &str) -> Result<Page> {
    let page = &data["me"]["libraryV3"];
    found(page, "LibraryPage", "part of the library")?;
    let items = list(page).filter_map(|i| {
        let (item, data) = (&i["item"], &i["item"]["data"]);
        match item["__typename"].as_str()? {
            "PlaylistResponseWrapper" => playlist(data),
            "AlbumResponseWrapper" => album(data),
            "ArtistResponseWrapper" => artist(data),
            "LibraryFolderResponseWrapper" => {
                Some(Hit { title: data["name"].as_str()?.into(), ..Hit::new(data["uri"].as_str()?) })
            }
            "LibraryPseudoPlaylistResponseWrapper" if item["_uri"] == "spotify:collection:tracks" => {
                Some(Hit { title: data["name"].as_str().unwrap_or("Liked Songs").into(), art: crate::spotify_search::art(data), ..Hit::new("liked") })
            }
            _ => None,
        }
    });
    // A folder is named by the last of its breadcrumbs.
    let folder = page["breadcrumbs"].as_array().and_then(|b| b.last()?["name"].as_str());
    let title = folder.unwrap_or(title);
    Ok(Page {
        title: title.into(),
        by: String::new(),
        sections: vec![Section::new(filter, page["totalCount"].as_u64(), items.collect())],
    })
}

fn liked_page(data: &Value) -> Result<Page> {
    let tracks = &data["me"]["library"]["tracks"];
    if !tracks.is_object() {
        bail!("Spotify did not give the Liked Songs");
    }
    // The track's URI is on its wrapper only.
    let items = list(tracks).filter_map(|i| {
        let mut data = i["track"]["data"].clone();
        data["uri"] = i["track"]["_uri"].clone();
        track(&data)
    });
    Ok(Page {
        title: "Liked Songs".into(),
        by: String::new(),
        sections: vec![Section::new("Tracks", tracks["totalCount"].as_u64(), items.collect())],
    })
}

fn top_page(data: &Value) -> Result<Page> {
    let profile = &data["me"]["profile"];
    if !profile.is_object() {
        bail!("Spotify did not give the top artists and tracks");
    }
    let (artists, tracks) = (&profile["topArtists"], &profile["topTracks"]);
    let top_artists = list(artists).filter_map(|i| artist(&i["data"])).collect();
    let top_tracks = list(tracks).filter_map(|i| track(&i["data"])).collect();
    Ok(Page {
        title: "Your top artists and tracks this month".into(),
        by: String::new(),
        sections: vec![
            Section::new("Artists", artists["totalCount"].as_u64(), top_artists),
            Section::new("Tracks", tracks["totalCount"].as_u64(), top_tracks),
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARTIST: &str = "spotify:artist:3WwGRA2o4Ux1RRMYaYDh7N";

    #[test]
    fn what_can_be_browsed() {
        assert_eq!(shelf(ARTIST).unwrap(), Shelf::Artist(ARTIST.into()));
        assert_eq!(shelf("https://open.spotify.com/intl-ja/artist/3WwGRA2o4Ux1RRMYaYDh7N?si=x").unwrap(), Shelf::Artist(ARTIST.into()));
        assert_eq!(
            shelf("https://open.spotify.com/album/3lBX7AtzE4JoZaAIBLptRx").unwrap(),
            Shelf::Album("spotify:album:3lBX7AtzE4JoZaAIBLptRx".into())
        );
        let folder = "spotify:user:12572401:folder:7e65e58f6872a79a";
        assert_eq!(shelf(folder).unwrap(), Shelf::Folder(folder.into()));
        assert_eq!(shelf(" Albums ").unwrap(), Shelf::Library("Albums", "Your albums"));
        assert_eq!(shelf("liked").unwrap(), Shelf::Liked);
        assert_eq!(shelf("spotify:collection").unwrap(), Shelf::Liked);
        assert_eq!(shelf("top").unwrap(), Shelf::Top);
        let refused = |input: &str| shelf(input).unwrap_err().to_string();
        assert_eq!(refused("spotify:track:7rU6Iebxzlvqy5t857bKFq"), "a track has nothing to browse: play it");
        assert!(refused("playlist").starts_with("browse takes"));
        assert!(refused("spotify:user:x:folder:not-hex").starts_with("not a Spotify folder"));
        assert!(refused("https://www.youtube.com/watch?v=dQw4w9WgXcQ").contains("only Spotify"));
    }

    fn track(name: &str, uri: &str, playable: bool) -> Value {
        json!({"name": name, "uri": uri, "duration": {"totalMilliseconds": 294493},
            "artists": {"items": [{"profile": {"name": "Mariya Takeuchi"}}]}, "playability": {"playable": playable}})
    }

    #[test]
    fn an_artist_page_lists_what_the_web_player_shows() {
        let overview = json!({"artistUnion": {"__typename": "Artist", "profile": {"name": "Mariya Takeuchi",
                "playlistsV2": {"totalCount": 0, "items": []}},
            "discography": {"topTracks": {"items": [
                {"track": track("Plastic Love", "spotify:track:7rU6Iebxzlvqy5t857bKFq", true)},
                {"track": track("Gone", "spotify:track:0000000000000000000000", false)}]}},
            "relatedContent": {"relatedArtists": {"totalCount": 40, "items": [
                {"uri": "spotify:artist:1LQQtqc1vQ1neUgZrjYlEU", "profile": {"name": "Yumi Matsutoya"}}]}}}});
        let releases = json!({"artistUnion": {"discography": {"all": {"totalCount": 10, "items": [
            {"releases": {"items": [{"name": "TRAD", "uri": "spotify:album:4h4LKGYNKz2gXaohJZKXY0", "type": "ALBUM",
                "date": {"isoString": "2014-09-26T00:00:00Z", "year": 2014}, "playability": {"playable": true}}]}}]}}}});
        let page = artist_page(&overview, &releases).unwrap();
        assert_eq!(page.title, "Mariya Takeuchi");
        let names: Vec<&str> = page.sections.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Popular", "Releases", "Fans also like"], "no playlists, so no section");
        assert_eq!(page.sections[0].items.len(), 1, "the unplayable track is left out");
        assert_eq!(page.sections[0].total, 1);
        assert_eq!(page.sections[1].total, 10);
        assert_eq!(page.sections[1].items[0], Hit { title: "TRAD".into(), year: Some(2014), ..Hit::new("spotify:album:4h4LKGYNKz2gXaohJZKXY0") });
        assert_eq!(page.sections[2].total, 40);
        assert_eq!(page.sections[2].items[0].target, "spotify:artist:1LQQtqc1vQ1neUgZrjYlEU");
        let missing = json!({"artistUnion": {"__typename": "NotFound"}});
        assert_eq!(artist_page(&missing, &releases).unwrap_err().to_string(), "Spotify has no such artist");
    }

    #[test]
    fn a_playlist_lists_its_songs_and_leaves_out_the_rest() {
        let data = json!({"playlistV2": {"__typename": "Playlist", "name": "80/90s Japanese City Pop",
            "ownerV2": {"data": {"name": "mert.uslu13 on Instagram"}},
            "content": {"totalCount": 56, "items": [
                {"itemV2": {"__typename": "TrackResponseWrapper", "data": {"name": "4:00A.M.", "uri": "spotify:track:2lV8YY0GQYXgtUWXM4NJ4X",
                    "trackDuration": {"totalMilliseconds": 336000}, "albumOfTrack": {"name": "Sunshower"},
                    "artists": {"items": [{"profile": {"name": "Taeko Onuki"}}]}, "playability": {"playable": true}}}},
                {"itemV2": {"__typename": "EpisodeOrChapterResponseWrapper", "data": {"name": "A talk", "uri": "spotify:episode:x"}}},
                {"itemV2": {"__typename": "LocalTrackResponseWrapper", "data": {"name": "Home tape", "uri": "spotify:local:x"}}}]}}});
        let page = playlist_page(&data).unwrap();
        assert_eq!((page.title.as_str(), page.by.as_str()), ("80/90s Japanese City Pop", "mert.uslu13 on Instagram"));
        assert_eq!(page.sections[0].total, 56);
        assert_eq!(page.sections[0].items, vec![Hit {
            title: "4:00A.M.".into(),
            artist: "Taeko Onuki".into(),
            album: Some("Sunshower".into()),
            duration_ms: Some(336000),
            ..Hit::new("spotify:track:2lV8YY0GQYXgtUWXM4NJ4X")
        }]);
        assert!(playlist_page(&json!({"playlistV2": {"__typename": "NotFound"}})).is_err());
    }

    #[test]
    fn the_library_lists_playlists_folders_and_liked_songs() {
        let data = json!({"me": {"libraryV3": {"__typename": "LibraryPage", "totalCount": 88, "items": [
            {"item": {"__typename": "LibraryPseudoPlaylistResponseWrapper", "_uri": "spotify:collection:tracks", "data": {"name": "Liked Songs"}}},
            {"item": {"__typename": "LibraryPseudoPlaylistResponseWrapper", "_uri": "spotify:collection:your-episodes", "data": {"name": "Your Episodes"}}},
            {"item": {"__typename": "PlaylistResponseWrapper", "_uri": "spotify:playlist:37i9dQZF1EQqA6klNdJvwx",
                "data": {"name": "Jazz Mix", "uri": "spotify:playlist:37i9dQZF1EQqA6klNdJvwx", "ownerV2": {"data": {"name": "Spotify"}}}}},
            {"item": {"__typename": "LibraryFolderResponseWrapper", "_uri": "spotify:user:12572401:folder:7e65e58f6872a79a",
                "data": {"name": "Grene", "uri": "spotify:user:12572401:folder:7e65e58f6872a79a", "playlistCount": 8}}},
            {"item": {"__typename": "AlbumResponseWrapper", "_uri": "spotify:album:14JkAa6IiFaOh5s0nMyMU9",
                "data": {"name": "KPop Demon Hunters", "uri": "spotify:album:14JkAa6IiFaOh5s0nMyMU9",
                    "artists": {"items": [{"profile": {"name": "HUNTR/X"}}]}, "date": {"isoString": "2025-06-20T00:00:00Z"}}}}]}}});
        let page = library_page(&data, "Playlists", "Your playlists").unwrap();
        assert_eq!(page.title, "Your playlists");
        let section = &page.sections[0];
        assert_eq!((section.name.as_str(), section.total), ("Playlists", 88));
        let targets: Vec<&str> = section.items.iter().map(|h| h.target.as_str()).collect();
        assert_eq!(targets, [
            "liked",
            "spotify:playlist:37i9dQZF1EQqA6klNdJvwx",
            "spotify:user:12572401:folder:7e65e58f6872a79a",
            "spotify:album:14JkAa6IiFaOh5s0nMyMU9"
        ]);
        assert_eq!(section.items[2].title, "Grene");
        assert_eq!(section.items[3].year, Some(2025), "read from the date when it has no year");

        let mut folder = data.clone();
        folder["me"]["libraryV3"]["breadcrumbs"] = json!([{"name": "Grene", "uri": "spotify:user:12572401:folder:7e65e58f6872a79a"}]);
        assert_eq!(library_page(&folder, "Playlists", "").unwrap().title, "Grene");
    }

    #[test]
    fn an_albums_tracks_take_its_cover() {
        let cover = |id: &str| json!({"sources": [{"url": format!("https://i.scdn.co/image/{id}"), "width": 640, "height": 640}]});
        let data = json!({"albumUnion": {"__typename": "Album", "name": "Expressions", "coverArt": cover("album"),
            "tracksV2": {"totalCount": 1, "items": [{"track": track("Plastic Love", "spotify:track:7rU6Iebxzlvqy5t857bKFq", true)}]}}});
        let page = album_page(&data).unwrap();
        assert_eq!(page.sections[0].items[0].art.as_deref(), Some("https://i.scdn.co/image/album"));
    }

    #[test]
    fn liked_songs_carry_their_uri_on_the_wrapper() {
        let data = json!({"me": {"library": {"tracks": {"totalCount": 9688, "items": [{"track": {"_uri": "spotify:track:2QihEF7BIfPCJjkB0c4Rrv",
            "data": {"name": "Boston", "duration": {"totalMilliseconds": 170859}, "albumOfTrack": {"name": "Long Way Home"},
                "artists": {"items": [{"profile": {"name": "STELLA LEFTY"}}]}, "playability": {"playable": true}}}}]}}}});
        let page = liked_page(&data).unwrap();
        assert_eq!(page.sections[0].total, 9688);
        assert_eq!(page.sections[0].items[0].target, "spotify:track:2QihEF7BIfPCJjkB0c4Rrv");
        assert!(liked_page(&json!({"me": null})).is_err());
    }
}
