#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET="${TARGET:-x86_64-pc-windows-gnu}"
MINGW_TARGET="${MINGW_TARGET:-x86_64-w64-mingw32}"
FFMPEG_PREFIX="${FFMPEG_PREFIX:-$ROOT_DIR/build/ffmpeg-win}"

export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER="$MINGW_TARGET-gcc"
export CC_x86_64_pc_windows_gnu="$MINGW_TARGET-gcc"
export AR_x86_64_pc_windows_gnu="$MINGW_TARGET-ar"
export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_PATH="$FFMPEG_PREFIX/lib/pkgconfig"

cd "$ROOT_DIR"
cargo build --release --target "$TARGET" -p stt-cli --features static-libav
cargo build --release --target "$TARGET" -p stt-gui \
    --features native-gui,static-libav

mkdir -p "$ROOT_DIR/dist/cli" "$ROOT_DIR/dist/gui"
cp "$ROOT_DIR/target/$TARGET/release/dictate-cli.exe" \
    "$ROOT_DIR/dist/cli/dictate-cli.exe"
cp "$ROOT_DIR/target/$TARGET/release/Dictate.exe" \
    "$ROOT_DIR/dist/gui/Dictate.exe"
