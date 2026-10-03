# Start-up performance

How fast the window opens, step by step, and what is left to gain.

## Current state (verified 2026-10-02, starship)

Medians in ms from the launch, with a local track playing and its cover
cached, on a quiet machine (load below 6). Release build.

| where | main | cover on screen |
|---|---|---|
| pseudo-terminal answering as kitty | | 20.8 (one frame) |
| pseudo-terminal, half blocks | | 6.7 |
| Ghostty 1.3.1, new process | 211 | 248 |
| Ghostty 1.3.1, warm (D-Bus `new-window-command`) | 92 | 128 |
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

## What is left

- **The image query waits 2 s on a terminal that answers nothing**
  (measured 2013 ms). ratatui-image 12.0.0-rc.0 keeps the timeout in
  `QueryStdioOptions`, which it does not export.
- **Kitty shared memory** would cut Ghostty's first draw from 31.5 to
  7.6 ms (measured with a local patch, not shipped). It needs the same
  private options.
- Encoding a cover for kitty: about 3.9 ms to resize (Triangle) and 3.4 ms
  for base64. Sixel: 176 KB per cover, about 23 ms to encode.
