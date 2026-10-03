# Spotify's web player queries

`search` and `browse` ask Spotify the way its web player does, with the
session's own sign-in, because the Web API's search answers 429 for
librespot's client id and a registered Web API app could no longer stream.
These queries are not a public API. This page records what AgentAmp sends,
read from the web player's code and checked against Spotify's answers.

## Current state (verified 2026-10-03)

Every query below answered 200 with the variables shown, for a Premium
account, from `src/pathfinder.rs` and the probes behind it.

| query | used for | variables (the web player's, paged by AgentAmp) | answer |
|---|---|---|---|
| `searchDesktop` | `search` | `searchTerm`, `offset` 0, `limit` the count, `numberOfTopResults` 5, `includeAudiobooks` true, `includeArtistHasConcertsField` false, `includePreReleases` true, `includeAlbumPreReleases` false, `includeAuthors` false, `includeEpisodeContentRatingsV2` true, `isPrefix` null, `sectionFilters` `["GENERIC"]` | `searchV2.{tracksV2,albumsV2,playlists,artists}` |
| `queryArtistOverview` | an artist's page | `uri`, `locale` "", `preReleaseV2` false | `artistUnion`: `discography.topTracks`, `profile.playlistsV2`, `relatedContent.relatedArtists` |
| `queryArtistDiscographyAll` | an artist's releases | `uri`, `offset`, `limit`, `order` `DATE_DESC` | `artistUnion.discography.all.items[].releases.items[0]` |
| `getAlbum` | an album | `uri`, `locale` "", `offset`, `limit` | `albumUnion`: name, artists, `tracksV2` |
| `fetchPlaylist` | a playlist | `uri`, `offset`, `limit`, `enableWatchFeedEntrypoint` false, `includeEpisodeContentRatingsV2` true | `playlistV2`: name, owner, `content.items[].itemV2` |
| `libraryV3` | `playlists`, `albums`, `artists`, a folder | `filters` `["Playlists"]` (or `Albums`, `Artists`), `order` null, `textFilter` null, `features` `["LIKED_SONGS","YOUR_EPISODES_V2","CLIPS","EVENTS"]`, `limit`, `offset`, `flatten` false, `expandedFolders` [], `folderUri` null or the folder, `includeFoldersWhenFlattening` true | `me.libraryV3`: `items[].item`, `totalCount`, `breadcrumbs` |
| `fetchLibraryTracks` | `liked` | `offset`, `limit` | `me.library.tracks.items[].track.{_uri,data}` |
| `userTopContent` | `top` | `includeTopArtists` and `includeTopTracks` true, each input `{offset, limit, sortBy: "AFFINITY", timeRange: "SHORT_TERM"}` | `me.profile.{topArtists,topTracks}` |

Measured on starship, 2026-10-03: 100 to 350 ms a query, and 860 ms for an
artist's page, whose two queries go together.

Playing an artist does not use these: `play spotify:artist:…` plays the
session's context for the artist, as Spotify's clients do (24 tracks in
Spotify's order, verified 2026-10-03).

## How a query is sent

`POST https://api-partner.spotify.com/pathfinder/v2/query` with

```json
{"operationName": "<query>", "variables": {...},
 "extensions": {"persistedQuery": {"version": 1, "sha256Hash": "<hash>"}}}
```

and the headers `Authorization: Bearer <login5 access token>`,
`client-token: <the session's client token>`, `app-platform: WebPlayer`
and `Content-Type: application/json`. The web player also sends
`Spotify-App-Version`, and `spotify-app-version` with `queryArtistOverview`
and `getAlbum`; AgentAmp sends neither, and every query above answered
without them.

The web player's code holds only each query's hash, never its text. Some
queries share one hash (`getAlbum` and `queryAlbumTracks`, `fetchPlaylist`
and `fetchPlaylistContents`, the four discography queries), so the right
`operationName` must go with it. A hash Spotify does not keep is answered
with HTTP 412.

## When the hashes change

AgentAmp carries the hashes above. When Spotify refuses one (412, or a
`PersistedQueryNotFound` error), it reads the web player's current code,
at most once an hour:

1. `GET https://open.spotify.com/` as a desktop browser. librespot's user
   agent is sent the mobile web player, whose code has none of these
   queries, so this one request goes through its own client.
2. The page loads `https://open.spotifycdn.com/cdn/build/web-player/web-player.<hex>.js`,
   which carries `queryArtistOverview`, `getAlbum`, `fetchPlaylist`,
   `libraryV3` and `fetchLibraryTracks` as
   `"<query>","query","<sha256>"`.
3. The others live in the chunks of single pages (`xpui-routes-search`,
   `xpui-routes-artist`, `xpui-routes-profile`). The main bundle names a
   chunk's file from two tables by chunk id:
   `({…,5021:"xpui-routes-search",…}[e]||e)+"."+({…,5021:"0197b7d2",…})[e]+".js"`.

The hashes found are kept in `~/.cache/agentamp/web-player-queries.json`
and the refused query is asked once more. Should Spotify change a query's
variables or answer rather than its hash, the fixtures in
`src/browse.rs` and `src/spotify_search.rs` show the shapes AgentAmp reads.
