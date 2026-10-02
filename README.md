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
```

`Cargo.lock` keeps `vergen` at 9.0.6: librespot-core 0.8's build script does
not compile against vergen 9.1.

## Use

```sh
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

YouTube links and `yt:` searches go through the installed `yt-dlp`. AgentAmp
downloads the AAC audio track once into `~/.cache/agentamp/youtube/` and
plays the file from there. This is for personal listening; downloading may
breach YouTube's terms. `AGENTAMP_YTDLP` points at another yt-dlp.

`AGENTAMP_HOME=<dir>` keeps every file under one directory, and
`AGENTAMP_AUDIO=null` plays silently while keeping time; the tests use both.
