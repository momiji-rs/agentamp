# Start-up performance

How fast the window opens, step by step, and what is left to gain.

## Current state (verified 2026-10-02, starship)

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

## How to measure

```sh
AGENTAMP_TRACE=<file> agentamp        # marks each step, µs since 1970
scripts/startup-bench.py --terminal kitty|sixel|plain|silent
scripts/terminal-bench.py --terminals ghostty,ghostty-warm,foot,alacritty
```

`startup-bench.py` plays the terminal in a pseudo-terminal, presses `/`
on the first frame and times the prompt. `terminal-bench.py` opens real
terminals on a headless Hyprland output, out of sight. Both read the
daemon at `AGENTAMP_HOME`; play a long track there first. Load from other
work skews both: check `uptime` and rerun on a quiet machine.

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

- Resizing a cover for kitty (Triangle) costs about 3.9 ms on the drawing
  thread, on a quiet machine. Sixel: 176 KB per cover, about 23 ms to
  encode.
- foot resizes the window twice after its first frame; the settled frame
  has not been timed on a quiet machine yet.
