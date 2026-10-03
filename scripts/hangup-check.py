#!/usr/bin/env python3
"""Checks that the window ends with its terminal.

    scripts/hangup-check.py [BINARY]

Opens the window in a pseudo-terminal, waits for its first frame, then
closes the terminal: once with the hang-up signal a terminal sends its
controlling process, once without it, as when the terminal goes away
some other way, and once more without it while stderr is that terminal
too. Each time the window must be gone within a second, having used no
CPU meanwhile. It runs against an empty AGENTAMP_HOME, so nothing plays
and no redraw can notice the closed terminal for it.
"""

import fcntl
import os
import pty
import select
import shutil
import subprocess
import sys
import termios
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ANSWER = b"\x1b[?62;22c\x1b[6;20;10t\x1b[0n"


def check(binary, home, signal, stderr_on_terminal):
    main, child = pty.openpty()

    def session():
        os.setsid()
        if signal:
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)

    env = dict(os.environ, TERM="xterm-256color", AGENTAMP_HOME=str(home))
    env.pop("TMUX", None)
    stderr = child if stderr_on_terminal else subprocess.DEVNULL
    process = subprocess.Popen([binary], stdin=child, stdout=child, stderr=stderr, env=env, preexec_fn=session)
    os.close(child)
    output, answered, deadline = b"", False, time.time() + 5
    while b"quit" not in output and time.time() < deadline:
        if select.select([main], [], [], 0.05)[0]:
            output += os.read(main, 1 << 16)
        if not answered and b"\x1b[5n" in output:
            os.write(main, ANSWER)
            answered = True
    os.close(main)
    time.sleep(1)
    code = process.poll()
    if code is None:
        ticks = sum(int(x) for x in Path(f"/proc/{process.pid}/stat").read_text().split()[13:15])
        process.kill()
        return f"still running after a second, {ticks} CPU ticks used"
    if code not in (0, -1):  # a clean end, or ended by the hang-up signal
        return f"ended with {code}"
    return None


def main():
    binary = sys.argv[1] if len(sys.argv) > 1 else str(ROOT / "target/release/agentamp")
    home = ROOT / "target/hangup-check"
    shutil.rmtree(home, ignore_errors=True)
    home.mkdir(parents=True)
    failed = False
    for signal, stderr, name in [(True, False, "hang-up signal"), (False, False, "no signal"), (False, True, "no signal, stderr on the terminal")]:
        problem = check(binary, home, signal, stderr)
        print(f"{name:<36} {problem or 'ok'}")
        failed |= problem is not None
    shutil.rmtree(home, ignore_errors=True)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
