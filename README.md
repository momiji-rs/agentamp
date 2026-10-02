# AgentAmp

A tiny terminal music player for Spotify, YouTube and your own music files,
built to be driven by agents as much as by hands.

- **Rust only, light on purpose.** One binary. It should play smoothly on a
  ten-year-old computer.
- **One queue, three sources.** Spotify (Premium, through
  [librespot](https://github.com/librespot-org/librespot)), YouTube (through
  an installed [yt-dlp](https://github.com/yt-dlp/yt-dlp)), and local MP3,
  FLAC, AAC, Ogg Vorbis and WAV files.
- **Agent native.** Every control is a CLI command with `--json` output, so an
  agent can act as your DJ.

AgentAmp is an early proof of concept.

## Development

```sh
cargo test
scripts/screens.sh   # the TUI tests' frames as PNGs, through termshot
```

TUI changes are checked by looking: `scripts/screens.sh` renders the frames
the tests keep in `target/screens/` with
[termshot](https://github.com/momiji-rs/termshot).

`Cargo.lock` keeps `vergen` at 9.0.6: librespot-core 0.8's build script does
not compile against vergen 9.1.

## Use

`agentamp` alone opens the window: your library, the queue and what is
playing, with the player bar below. Space plays and pauses, `n` skips, `s`
stops, the arrows seek and set the volume, `/` plays a link, search or
path, `a` adds one, and `q` closes the window while the music keeps going.
`agentamp tui --frame 120x36` prints one frame as terminal output, for
scripts, agents and screenshots.

```sh
agentamp login                     # once, for Spotify
agentamp play https://open.spotify.com/album/…
agentamp play ~/Music/Album        # a folder, a file, or a link
agentamp play yt: plastic love     # the first YouTube result
agentamp add --next song.flac      # after the current track
agentamp now                       # ▶ Title · Artist  1:23 / 4:29
agentamp --json now                # the same, for scripts and agents
agentamp pause | resume | toggle | next | stop | clear
agentamp queue
agentamp seek 1:30
agentamp volume 40
agentamp quit
```

The first command starts a small background player, which keeps playing
after the terminal closes. It listens on a Unix socket in the runtime
directory (`$XDG_RUNTIME_DIR/agentamp.sock`) that only your user can open.
`now` and `queue` never start it. Its log is `~/.cache/agentamp/agentamp.log`.

Spotify needs a Premium account. `agentamp login` opens Spotify's sign-in
page in the browser and keeps librespot's reusable credential in
`~/.config/agentamp/credentials.json`, in a directory only your user can
read; `agentamp logout` deletes it. Track, album and playlist links and
`spotify:` URIs play. Spotify audio is never saved as music files: librespot
keeps up to 2 GiB of it in its own encrypted cache
(`~/.cache/agentamp/spotify-audio/`), so songs heard again are not
downloaded again.

YouTube links and `yt:` searches go through the installed `yt-dlp`. AgentAmp
downloads the AAC audio track once into `~/.cache/agentamp/youtube/` and
plays the file from there. This is for personal listening; downloading may
breach YouTube's terms. `AGENTAMP_YTDLP` points at another yt-dlp.

`AGENTAMP_HOME=<dir>` keeps every file under one directory, and
`AGENTAMP_AUDIO=null` plays silently while keeping time; the tests use both.
