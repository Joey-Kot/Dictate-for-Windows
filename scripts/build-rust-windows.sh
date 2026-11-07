#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARCH="${1:-amd64}"
DIST_DIR="${DIST_DIR:-$ROOT_DIR/dist}"
case "$ARCH" in
    amd64)
        default_target=x86_64-pc-windows-gnu
        default_mingw_target=x86_64-w64-mingw32
        default_prefix="$ROOT_DIR/build/ffmpeg-win"
        binary_dir="$DIST_DIR"
        pointer_bytes=8
        object_format=pe-x86-64
        ;;
    x86)
        default_target=i686-pc-windows-gnu
        default_mingw_target=i686-w64-mingw32
        default_prefix="$ROOT_DIR/build/ffmpeg-win-x86"
        binary_dir="$DIST_DIR/x86"
        pointer_bytes=4
        object_format=pe-i386
        ;;
    *)
        echo "Usage: $0 [amd64|x86]" >&2
        exit 2
        ;;
esac
TARGET="${TARGET:-$default_target}"
MINGW_TARGET="${MINGW_TARGET:-$default_mingw_target}"
FFMPEG_PREFIX="${FFMPEG_PREFIX:-$default_prefix}"

if [[ "$TARGET" != "$default_target" ]]; then
    echo "Rust target $TARGET does not match $ARCH" >&2
    exit 1
fi
actual_pointer_bytes="$(printf '' | "$MINGW_TARGET-gcc" -dM -E - | awk '$2 == "__SIZEOF_POINTER__" { print $3 }')"
if [[ "$actual_pointer_bytes" != "$pointer_bytes" ]]; then
    echo "$MINGW_TARGET-gcc does not target $ARCH" >&2
    exit 1
fi
if ! "$MINGW_TARGET-objdump" -f "$FFMPEG_PREFIX/lib/libavcodec.a" | grep -F "file format $object_format" >/dev/null; then
    echo "$FFMPEG_PREFIX does not contain $ARCH FFmpeg libraries" >&2
    exit 1
fi

case "$ARCH" in
    amd64)
        export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER="$MINGW_TARGET-gcc"
        export CC_x86_64_pc_windows_gnu="$MINGW_TARGET-gcc"
        export AR_x86_64_pc_windows_gnu="$MINGW_TARGET-ar"
        ;;
    x86)
        export CARGO_TARGET_I686_PC_WINDOWS_GNU_LINKER="$MINGW_TARGET-gcc"
        export CC_i686_pc_windows_gnu="$MINGW_TARGET-gcc"
        export AR_i686_pc_windows_gnu="$MINGW_TARGET-ar"
        ;;
esac
export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_PATH="$FFMPEG_PREFIX/lib/pkgconfig"
export PKG_CONFIG_LIBDIR="$FFMPEG_PREFIX/lib/pkgconfig"

cd "$ROOT_DIR"
cargo build --release --target "$TARGET" -p dictate-cli --features static-libav
cargo build --release --target "$TARGET" -p dictate-gui \
    --features native-gui,static-libav

mkdir -p "$binary_dir/cli" "$binary_dir/gui"
cp "$ROOT_DIR/target/$TARGET/release/dictate-cli.exe" \
    "$binary_dir/cli/dictate-cli.exe"
cp "$ROOT_DIR/target/$TARGET/release/Dictate.exe" \
    "$binary_dir/gui/Dictate.exe"
