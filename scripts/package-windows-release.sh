#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST_DIR="${DIST_DIR:-$ROOT_DIR/dist}"

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
    python3 -m zipfile -c "$DIST_DIR/$archive" \
        "$executable" \
        README.md \
        LICENSE \
        NOTICE \
        THIRD_PARTY_NOTICES.txt \
        THIRD_PARTY_LICENSES
}

package "$DIST_DIR/cli" dictate-cli.exe dictate-cli-windows-amd64.zip
package "$DIST_DIR/gui" Dictate.exe dictate-gui-windows-amd64.zip

cd "$DIST_DIR"
sha256sum dictate-cli-windows-amd64.zip > dictate-cli-windows-amd64.zip.sha256
sha256sum dictate-gui-windows-amd64.zip > dictate-gui-windows-amd64.zip.sha256
