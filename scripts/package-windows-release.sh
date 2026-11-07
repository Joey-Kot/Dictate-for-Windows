#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST_DIR="${DIST_DIR:-$ROOT_DIR/dist}"
ARCH="${1:-amd64}"
case "$ARCH" in
    amd64) binary_dir="$DIST_DIR" ;;
    x86) binary_dir="$DIST_DIR/x86" ;;
    *)
        echo "Usage: $0 [amd64|x86]" >&2
        exit 2
        ;;
esac

package() {
    local source_dir="$1"
    local executable="$2"
    local archive="$3"

    cp "$ROOT_DIR/README.md" \
        "$ROOT_DIR/LICENSE" \
        "$ROOT_DIR/NOTICE" \
        "$ROOT_DIR/THIRD_PARTY_NOTICES.txt" \
        "$source_dir/"
    rm -rf "$source_dir/THIRD_PARTY_LICENSES"
    cp -R "$ROOT_DIR/THIRD_PARTY_LICENSES" "$source_dir/"

    cd "$source_dir"
    python3 -B -m zipfile -c "$DIST_DIR/$archive" \
        "$executable" \
        README.md \
        LICENSE \
        NOTICE \
        THIRD_PARTY_NOTICES.txt \
        THIRD_PARTY_LICENSES
}

package "$binary_dir/cli" dictate-cli.exe "dictate-cli-windows-$ARCH.zip"
package "$binary_dir/gui" Dictate.exe "dictate-gui-windows-$ARCH.zip"

cd "$DIST_DIR"
sha256sum "dictate-cli-windows-$ARCH.zip" > "dictate-cli-windows-$ARCH.zip.sha256"
sha256sum "dictate-gui-windows-$ARCH.zip" > "dictate-gui-windows-$ARCH.zip.sha256"
