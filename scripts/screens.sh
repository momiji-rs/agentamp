#!/bin/sh
# Renders the frames the TUI tests keep in target/screens/ into PNGs with
# termshot (https://github.com/momiji-rs/termshot). TERMSHOT names the binary.
set -eu
cd "$(dirname "$0")/.."
termshot=${TERMSHOT:-termshot}
cargo test --quiet tui:: >/dev/null
mkdir -p target/screens/png
for frame in target/screens/*.ansi; do
    name=$(basename "$frame" .ansi)
    "$termshot" --size "${name##*.}" --px 24 "$frame" "target/screens/png/$name.png"
done
ls target/screens/png
