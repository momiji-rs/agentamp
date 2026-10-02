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
