#!/usr/bin/env python3
"""Times the window's start in real terminals, on a Hyprland desktop.

    scripts/terminal-bench.py [--runs 10] [--terminals ghostty,ghostty-warm,foot,alacritty] [BINARY]

Each run opens a terminal running the window on a headless output, out
of the user's sight (workspace 6, silently), and reads its AGENTAMP_TRACE
marks. Times are milliseconds from asking Hyprland to launch the terminal:
the median and the slowest run. The terminal's own start counts, since a
user waits for it too; what it takes the terminal to put the last frame on
glass is not measured. `cover drawn` is the last frame with the cover,
after any resize the terminal made once the window was up.

`ghostty-warm` opens each window in a Ghostty that is already running,
through its `new-window-command` D-Bus action: `ghostty -e` always starts
a new process, single instance or not. The harness starts that Ghostty
itself, under its own class, and leaves it running.
"""

import argparse
import os
import shlex
import statistics
import subprocess
import time
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUTPUT = "agentamp-shot"
CLASS = "agentamp.shot"
# The running Ghostty's class: one per application, so the cold runs of
# `ghostty` must not share it.
WARM = "agentamp.warm"
SINGLE = "--gtk-single-instance=true"

# (command, window class). A running Ghostty starts the window's command in
# its own environment, so the warm command carries the variables itself.
TERMINALS = {
    "ghostty": ("ghostty --class={cls} -e {cmd}", CLASS),
    "ghostty-warm": (
        "busctl --user call -- {cls} /{path} org.gtk.Actions Activate 'sava{{sv}}' new-window-command 1 as {argv} 0",
        WARM,
    ),
    "foot": ("foot --app-id={cls} {cmd}", CLASS),
    "alacritty": ("alacritty --class {cls} -e {cmd}", CLASS),
}


def hypr(*args):
    return subprocess.run(["hyprctl", *args], capture_output=True, text=True, check=True).stdout


def lua(code):
    return hypr("eval", code)


def now_us():
    return time.time_ns() // 1000


def setup(terminals):
    if OUTPUT not in hypr("monitors", "all"):
        hypr("output", "create", "headless", OUTPUT)
    for cls in (CLASS, WARM):
        lua(f"hl.window_rule({{ match = {{ class = '{cls.replace('.', '[.]')}' }}, workspace = '6 silent' }})")
    if "ghostty-warm" in terminals and subprocess.run(["pgrep", "-f", f"class={WARM}"], capture_output=True).returncode:
        lua(f"hl.exec_cmd('ghostty --class={WARM} {SINGLE} --initial-window=false --quit-after-last-window-closed=false')")
        time.sleep(2)


def close(cls):
    lua(f"hl.dispatch(hl.dsp.window.close({{ window = 'class:{cls}' }}))")
    for _ in range(100):
        if f"class: {cls}" not in hypr("clients"):
            return
        time.sleep(0.02)


# How long the trace must stay quiet after the cover is drawn: a terminal
# may resize the window after its first frame, and the frame after the
# last resize is the one the user sees.
SETTLE = 0.3


def drawn(lines):
    """The last frame after the cover was applied, if any."""
    applied = next((i for i, line in enumerate(lines) if line.endswith("applied art")), None)
    if applied is None:
        return None
    return next((line for line in reversed(lines[applied:]) if line.split(" ")[1] == "frame"), None)


def run_once(binary, terminal, trace):
    trace.unlink(missing_ok=True)
    # Hyprland starts the terminal with its own environment, not ours.
    env = f"AGENTAMP_TRACE={shlex.quote(str(trace))}"
    if "AGENTAMP_HOME" in os.environ:
        env += f" AGENTAMP_HOME={shlex.quote(os.path.abspath(os.environ['AGENTAMP_HOME']))}"
    template, cls = TERMINALS[terminal]
    argv = ["-e", "env", *env.split(" "), binary]
    argv = f"{len(argv)} " + " ".join(shlex.quote(a) for a in argv)
    path = cls.replace(".", "/")
    command = f"{env} " + template.format(cls=cls, path=path, argv=argv, cmd=shlex.quote(binary))
    launched = now_us()
    lua(f"hl.exec_cmd({command!r})".replace("\\'", "'"))
    lines, changed = [], time.time()
    deadline = changed + 6
    while time.time() < deadline:
        now = trace.read_text().splitlines() if trace.exists() else []
        if now != lines:
            lines, changed = now, time.time()
        if drawn(lines) and time.time() - changed > SETTLE:
            break
        time.sleep(0.005)
    close(cls)
    times = {}
    for line in lines:
        at, step = line.split(" ", 1)
        words = step.split(" ")
        if words[0] == "frame":
            step = f"frame {words[1]} {words[3]}"
            times[step + " draw"] = int(words[2][:-2]) / 1000
        elif words[0] == "image":
            step = f"image {words[2]}"
            times[step + " cost"] = int(words[3][:-2]) / 1000
        elif words[0] == "resize":
            step = "resize"
        times.setdefault(step, (int(at) - launched) / 1000)
    last = drawn(lines)
    times["resizes"] = sum(line.split(" ")[1] == "resize" for line in lines)
    times["cover drawn"] = (int(last.split(" ")[0]) - launched) / 1000 if last else None
    return times


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", nargs="?", default=str(ROOT / "target/release/agentamp"))
    parser.add_argument("--runs", type=int, default=10)
    parser.add_argument("--terminals", default="ghostty,foot,alacritty")
    args = parser.parse_args()
    # Hyprland starts the terminal in its own directory, not ours.
    args.binary = os.path.abspath(args.binary)
    trace = ROOT / "target/terminal-bench.trace"
    setup(args.terminals.split(","))
    for terminal in args.terminals.split(","):
        runs, order = defaultdict(list), []
        for _ in range(args.runs):
            for key, value in run_once(args.binary, terminal, trace).items():
                if key not in order:
                    order.append(key)
                if value is not None:
                    runs[key].append(value)
            time.sleep(0.3)
        print(f"{terminal}, {args.runs} runs: ms from launch, median / slowest")
        for key in order:
            values = runs[key]
            if not values:
                print(f"  {key:<28} never")
                continue
            if key == "resizes":
                print(f"  {key:<28} {statistics.median(values):>9.0f} {max(values):>9.0f}")
                continue
            print(f"  {key:<28} {statistics.median(values):>9.2f} {max(values):>9.2f}  ({len(values)})")
    trace.unlink(missing_ok=True)


if __name__ == "__main__":
    os.environ.setdefault("WAYLAND_DISPLAY", "wayland-1")
    if "HYPRLAND_INSTANCE_SIGNATURE" not in os.environ:
        runtime = Path(f"/run/user/{os.getuid()}/hypr")
        os.environ["HYPRLAND_INSTANCE_SIGNATURE"] = sorted(runtime.iterdir())[0].name
    main()
