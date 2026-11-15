#!/usr/bin/env bash
# Launch romgetter full-screen in Konsole so it can run as a non-Steam game in Game Mode.
#
# Put this next to the romgetter binary on the Deck (e.g. ~/romgetter and
# ~/romgetter-launch.sh), chmod +x both, then in Desktop Mode:
#   Steam → Add a Game → Add a Non-Steam Game → Browse → pick this script.
#
# Env overrides:
#   ROMGETTER_BIN        path to the binary (default: ./romgetter next to this script, then ~/romgetter)
#   ROMGETTER_FONT_SIZE  terminal font size in points (default: 15)
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
bin="${ROMGETTER_BIN:-}"
if [[ -z "$bin" ]]; then
    for c in "$here/romgetter" "$here/romgetter-steamdeck" "$HOME/romgetter"; do
        if [[ -x "$c" ]]; then bin="$c"; break; fi
    done
fi
if [[ -z "$bin" || ! -x "$bin" ]]; then
    echo "romgetter binary not found (set ROMGETTER_BIN)" >&2
    exit 1
fi

# A dedicated Konsole profile: big font, no scrollbar, no bell. Rewritten every launch so
# changing ROMGETTER_FONT_SIZE takes effect.
profile_dir="${XDG_DATA_HOME:-$HOME/.local/share}/konsole"
mkdir -p "$profile_dir"
cat >"$profile_dir/romgetter.profile" <<EOF
[Appearance]
ColorScheme=Breeze
Font=Hack,${ROMGETTER_FONT_SIZE:-15},-1,5,50,0,0,0,0,0

[General]
Name=romgetter
Parent=FALLBACK/
SilenceSeconds=0

[Scrolling]
HistoryMode=0
ScrollBarPosition=2

[Terminal Features]
BellMode=3
EOF

# If romgetter exits with an error, keep the window open so the message can be read
# (press A/Enter to return to Steam).
exec konsole --separate --profile romgetter --fullscreen --hide-menubar --hide-tabbar \
    -e bash -c '"$1" || { echo; read -rp "romgetter exited with an error. Press Enter to close."; }' _ "$bin"
