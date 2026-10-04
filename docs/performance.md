# Performance

How fast the window opens, step by step, what it costs while it plays, and
what is left to gain.

## Current state (verified 2026-10-02, Arch Linux, Ryzen 7 8745HS, Hyprland)

Medians in ms from the launch, with a local track playing and its cover
cached, on a quiet machine (load below 6). Release build.

| where | main | cover on screen |
|---|---|---|
| pseudo-terminal answering as kitty | | 20.8 (one frame) |
| pseudo-terminal, half blocks | | 6.7 |
| Ghostty 1.3.1, new process | 211 | 248 (before shared memory) |
| Ghostty 1.3.1, warm (D-Bus `new-window-command`) | 92 | 128 (before shared memory) |
| foot | 24.6 | 50, first frame (foot then resizes the window) |
| Alacritty (half blocks) | | 93 |

- The window draws one complete frame: it waits up to 50 ms for the
  player's state and the playing track's cover, which come from this
  machine in a few ms. A cover still on the network is not waited for.
- A key works as soon as the first frame is out (`input ready`).
- Most of the time in a real terminal is the terminal's own start. A
  launcher that opens the window in a running Ghostty saves about 120 ms.
- While a song plays, the spectrum redraws at 60 fps: the window takes
  3.8% of one core and Ghostty 6.9% drawing it. Paused, once the bars have
  fallen, the window takes 0% and Ghostty 0.4%.
- `play` and `add` with a YouTube target answer in 2 to 3 ms (29 ms when
  the call starts the player) and download behind; before, they took 1.5 to
  4.5 s. The sound itself waits for yt-dlp, about 2.5 s, the first time
  only: a link or search asked for again is ready in 5 to 6 ms (2026-10-03).
- As the queue and the play log grow (benchmarks, 2026-10-04): the
  window's look at the queue, every 500 ms, costs 0.41 ms at 10 000
  queued tracks (the size of the Liked Songs) and 0.51 ms at 100 000, as
  it asks for the first 200 upcoming tracks and the whole queue's count
  and length. Filling in a batch of 10 tracks' details walks the queue
  once: about 0.23 ms at 10 000. Keeping one play takes 0.93 ms whatever
  the log's size. See below.
- `agentamp sync` of 9 688 Liked Songs and their 1 130 albums takes 5.8 s
  into an empty database and 4.2 s again, in 13 requests (2026-10-04).

## How to measure

```sh
AGENTAMP_TRACE=<file> agentamp        # marks each step, µs since 1970
cargo build --release
cargo run --release --example startup_bench -- --terminal kitty|sixel|plain|silent
cargo run --release --example terminal_bench -- --terminals ghostty,ghostty-warm,foot,alacritty
```

`startup_bench` plays the terminal in a pseudo-terminal, presses `/`
on the first frame and times the prompt. `terminal_bench` opens real
terminals on a headless Hyprland output, out of sight. Both read the
daemon at `AGENTAMP_HOME`; play a long track there first. Load from other
work skews both: check `uptime` and rerun on a quiet machine.

```sh
cargo bench -- --save-baseline main   # the queue and the database at 1k, 10k, 100k
cargo bench -- --baseline main        # a change against it
```

The benchmarks (`benches/`) build the same made-up library every run,
shaped like a real one (about 3.7 songs an artist, a few artists holding
most), and keep their files in `target/bench-scratch/`.

`tests/scaling.rs` runs in `cargo test`, so in CI on Linux and macOS: it
times the same cases at 1 000 and 10 000 and fails when ten times the data
costs more than 20 times the time (3 times for what an index or a fixed
window answers), or when a case leaves its budget at 10 000. The budgets
are four to five times a debug build on starship. A queue sent whole to
the window again fails it at ×10.2 (2026-10-04).

## While playing (2026-10-02)

Release build in Ghostty 1.3.1, full screen on a 1920×1080 headless
output (the Now playing panel's spectrum is 11 bars by 8 rows), a local
AAC file at volume 0, load about 2.4. CPU from `/proc/<pid>/stat`
(utime + stime) over three 10 s windows: the window 38, 38 and 39 ticks of
1000, Ghostty 69, 69 and 70. Paused for 10 s: the window 0, Ghostty 4.
Nearly all of the window's time is on its drawing thread; the thread that
reads the sound from the player stays under one tick in 5 s.

## As the library grows (2026-10-04)

Criterion, release build, the median of each benchmark's run, load about
1.2 (rustc 1.98.1). The database is on the btrfs home disk.

| benchmark | 1 000 | 10 000 | 100 000 |
|---|---|---|---|
| queue/snapshot: the state the window asks for, encoded and decoded | 1.97 ms | 26.5 ms | 298 ms |
| queue/details: 200 tracks' details put in the queue | 0.43 ms | 4.68 ms | 160 ms |
| queue/downloads: what the engine checks after each message | 0.42 µs | 4.67 µs | 80.3 µs |
| queue/replace: `play` with that many tracks | 38.8 µs | 399 µs | 7.02 ms |
| db/insert: one play kept, in the log of that many | 0.93 ms | 0.93 ms | 0.93 ms |
| db/open | 200 µs | 204 µs | 202 µs |
| db/top_artists: every play grouped | 0.19 ms | 2.19 ms | 28.7 ms |
| db/by_month: every play grouped | 0.11 ms | 1.08 ms | 16.8 ms |
| db/recent: the last 20, by index | 6.0 µs | 6.1 µs | 6.8 µs |
| db/one_song: one track's plays, by index | 4.7 µs | 4.8 µs | 4.9 µs |
| db/import: that many plays in one transaction | 7.70 ms | 50.3 ms | 657 ms |

- The snapshot grows with the queue and is paid twice a second while the
  window is open: at 100 000 tracks it takes longer than the time
  between two asks.
- Details grow faster than the queue: each of the 200 tracks walks the
  whole queue, and at 100 000 the queue no longer fits the cache.
- Keeping a play is the disk's flush, not the log's size.
- Questions over every play grow linearly and stay under 30 ms at
  100 000 plays; the indexed ones do not grow.

### The window's look and the details, after (2026-10-04)

The daemon answers the window with a stretch of the queue, and details
arriving in a batch are filled in with one walk. Against the table above,
load about 3.8:

| benchmark | 1 000 | 10 000 | 100 000 |
|---|---|---|---|
| queue/snapshot | 0.40 ms (−80%) | 0.41 ms (−98%) | 0.51 ms (−99.8%) |
| queue/details | 0.43 ms | 4.65 ms | 112 ms (−30%) |

The details still grow with the queue: each batch of 10 walks it once, a
hash lookup a track where it was ten string comparisons. Doing better
needs an index of where each track sits, which the queue's moves would
have to keep. At 10 000 a batch costs about 0.23 ms on the engine, so it
stays as it is. `downloads` and `replace`, unchanged, measured 6 to 8%
slower at 100 000 in the same run, under the higher load.

## Under load: 100 000 tracks and eight windows (2026-10-04)

`cargo test --release --test stress -- --ignored --nocapture` starts a
daemon with silent audio, plays a folder of 10 000 WAV files, adds it 9
times more, then has eight windows ask for the status and 200 of the
queue as fast as they can for 10 s, while an agent adds a track every
100 ms. Load about 5:

- `play` of the folder answers in 71 ms, each `add` of it in 60 ms.
- The daemon holds 9 MiB idle, 13 MiB with 10 000 queued, 39 MiB with
  99 999.
- The windows wait 3.0 ms (median) and 6.7 ms (p99), 26 000 asks in 10 s,
  at 1.3 cores: a real window asks twice a second. Each add takes 1.5 ms.
- When the agent also reads the whole queue after each add, a read takes
  118 ms on the daemon, and the windows' p99 goes to 83 ms (slowest
  160 ms): the daemon answers one request at a time.
- `agentamp queue` now reads it 1 000 tracks at a time. With the agent
  running it after each add, the windows' p99 is 10.5 ms (slowest
  15.9 ms; 8.6 ms with adds alone, in the same run at load about 3), and
  the listing of 100 000 tracks takes 470 ms, its pages waiting behind
  the eight windows'.

## Syncing the Liked Songs (2026-10-04)

9 688 Liked Songs of 1 130 albums, debug build, load about 15 (another
session's build).

- The songs: `fetchLibraryTracks` gives at most 1 000 a request (10 000
  gives none), 0.9 s each; the first page says how many there are and the
  other nine go four at a time. A page short of its share stops the sync
  before the table is touched.
- The albums: a liked song has no release date. `getAlbum` has it, at
  about 270 ms an album, but librespot lets a session make 300 requests
  in 30 s to spotify.com and refuses the rest on its own side ("rate
  limited"): eight at a time, 734 of 1 130 were refused, and while it
  lasts playback and browsing are refused too. The catalogue's extended
  metadata takes many albums to a request: 500 in 0.3 s, every one dated
  and labelled. The 1 130 take 3 requests.
- A sync into an empty database: 5.8 s. Again, nothing new: 4.2 s, all of
  it Spotify's answers. Writing the table is 193 ms at 10 000 songs in a
  debug build (`tests/scaling.rs`).
- Of the albums, 1 044 are dated to the day, 2 to the month, 84 to the
  year only.

## YouTube play and add (2026-10-03)

Release build, `AGENTAMP_AUDIO=null`, a fresh `AGENTAMP_HOME`, yt-dlp
2026.08.19 on the home connection, ms by `date +%s%3N` around each call.

| step | before | after |
|---|---|---|
| `play yt: …`, answered (starts the player) | 1,770 | 29 |
| …until the file plays | 1,770 | 2,619 |
| `add yt: …`, answered | 1,520 and 4,510 | 3 and 2 |
| both added files ready, two at a time | | 2,533 after the second add |
| `add` of a search already cached, answered | 1,250 | 3 |
| …until ready, yt-dlp searching again | 1,250 | 2,482 |
| …until ready, remembering the search (three runs) | | 6, 5 and 6 |

The before column came from other searches on an earlier run, so compare
the answers, not the time to sound: that is yt-dlp's search and download,
which vary from one run to the next, as the two before adds show. What
changed is that nothing waits for it, and that a link or search asked for
again skips yt-dlp: each one's answer is kept in `youtube/found/` next to
the files. The player held 12 MB resident afterwards.

## What was fixed (2026-10-02)

Cover on screen, ms from launch, before and after asking for the state at
once and waking on updates instead of a 500 ms poll and a 50 ms input tick:

| terminal | before | after |
|---|---|---|
| Ghostty | 859 | 243 |
| foot | 655 | 133 |
| Alacritty | 713 | 93 |
| pseudo-terminal | 613 | 12 |

Then:

- One complete first frame instead of three that filled in.
- A cover is encoded once per place, not on every frame, whatever the
  cell shape: foot's later frames went from a fresh Sixel encode to
  0.37 ms.
- A terminal closed without a hang-up signal ends the window, instead of
  leaving it spinning at full CPU (crossterm-rs/crossterm#793).
  `tests/hangup.rs` checks it on Linux, with and without the signal.
- A terminal that never answers the image query now holds the first frame
  for 500 ms, not ratatui-image's default 2 s (2017 ms to 519 ms, in a
  pseudo-terminal that answers nothing). Every terminal answers the
  query's last question, so the cap only bounds a silent one; over SSH a
  long link still has a round trip to spare.
- Covers go to kitty-protocol terminals through shared memory where the
  terminal reads it back, and as base64 elsewhere (over SSH, for one).
  Ghostty's first frame with a cover drew in 21 to 41 ms instead of 94 to
  149 ms (under load, the two builds alternated); no objects were left in
  `/dev/shm`. Both use ratatui-image's `QueryStdioOptions`, which is
  public, though its doc said to use it only for the Text Sizing Protocol;
  that doc is fixed upstream in
  ratatui/ratatui-image#216 with the measurement in
  [linyiru/ratatui-image-query-options](https://github.com/linyiru/ratatui-image-query-options).

## What is left

- Details: an index of where each track sits in the queue, should a
  batch's walk ever show at 100 000 tracks.

- Resizing a cover for kitty (Triangle) costs about 3.9 ms on the drawing
  thread, on a quiet machine. Sixel: 176 KB per cover, about 23 ms to
  encode.
- foot resizes the window twice after its first frame; the settled frame
  has not been timed on a quiet machine yet.
