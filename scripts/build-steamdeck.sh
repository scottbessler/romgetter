#!/usr/bin/env bash
# Build a static x86-64 Linux binary for the Steam Deck (SteamOS) from macOS or Linux.
#
# One-time setup:
#   brew install zig
#   cargo install --locked cargo-zigbuild
#   rustup target add x86_64-unknown-linux-musl
#
# Output: dist/romgetter-steamdeck (no runtime dependencies; musl, statically linked)
set -euo pipefail
cd "$(dirname "$0")/.."

target=x86_64-unknown-linux-musl
cargo zigbuild --release --target "$target"

mkdir -p dist
cp "target/$target/release/romgetter" dist/romgetter-steamdeck
cp scripts/steamdeck-launch.sh dist/romgetter-launch.sh
chmod +x dist/romgetter-steamdeck dist/romgetter-launch.sh
file dist/romgetter-steamdeck 2>/dev/null || true
echo
echo "Built dist/romgetter-steamdeck and dist/romgetter-launch.sh"
echo "Copy them to the Deck, e.g.:"
echo "  scp dist/romgetter-steamdeck deck@steamdeck:~/romgetter"
echo "  scp dist/romgetter-launch.sh deck@steamdeck:~/romgetter-launch.sh"
