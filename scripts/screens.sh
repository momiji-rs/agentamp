#!/bin/sh
# Renders the frames the TUI tests keep in target/screens/ into PNGs with
# termshot (https://github.com/momiji-rs/termshot). TERMSHOT names the binary;
# the font is JetBrainsMono Nerd Font, Omarchy's monospace, when installed,
# so the icons look as they do there. TERMSHOT_FONT names another, and
# TERMSHOT_PX the font's pixel height: 24 to look at, 48 for a page read on a
# 2x screen, which would otherwise enlarge a 24 px image and blur it.
set -eu
cd "$(dirname "$0")/.."
termshot=${TERMSHOT:-termshot}
cargo test --quiet tui:: >/dev/null
font=${TERMSHOT_FONT:-$(fc-match -f '%{file}' 'JetBrainsMono Nerd Font' 2>/dev/null || true)}
case "$font" in *.ttf) set -- --font "$font" ;; *) set -- ;; esac
mkdir -p target/screens/png
for frame in target/screens/*.ansi; do
    name=$(basename "$frame" .ansi)
    "$termshot" "$@" --size "${name##*.}" --px "${TERMSHOT_PX:-24}" "$frame" "target/screens/png/$name.png"
done
ls target/screens/png
