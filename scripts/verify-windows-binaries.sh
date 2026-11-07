#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST_DIR="${DIST_DIR:-$ROOT_DIR/dist}"
ARCH="${1:-amd64}"
case "$ARCH" in
    amd64)
        binary_dir="$DIST_DIR"
        objdump=x86_64-w64-mingw32-objdump
        pe_format=pei-x86-64
        ;;
    x86)
        binary_dir="$DIST_DIR/x86"
        objdump=i686-w64-mingw32-objdump
        pe_format=pei-i386
        ;;
    *)
        echo "Usage: $0 [amd64|x86]" >&2
        exit 2
        ;;
esac

gui="$binary_dir/gui/Dictate.exe"
cli="$binary_dir/cli/dictate-cli.exe"
for binary in "$gui" "$cli"; do
    "$objdump" -f "$binary" | grep -F "file format $pe_format"
    "$objdump" -x "$binary" | grep -F 'keybd_event'
    "$objdump" -x "$binary" | grep -F 'SendInput'
    if strings "$binary" | grep -F '[ffmpeg] executing: ffmpeg'; then
        echo "$binary contains the external FFmpeg backend" >&2
        exit 1
    fi
    strings "$binary" | grep -F '[ffmpeg] crop/transcode'
    imports="$("$objdump" -p "$binary" | grep 'DLL Name')"
    echo "$imports"
    if echo "$imports" | grep -E -i 'avcodec|avformat|avutil|avdevice|avfilter|swscale|swresample|portaudio|opus|vorbis|ogg|mp3lame|speex|opencore|vo-amrwbenc|libatomic|libgcc|libstdc\+\+|libwinpthread'; then
        echo "$binary has a dynamic audio or compiler runtime dependency" >&2
        exit 1
    fi
done

# Subclass helpers must resolve against Common Controls v6 at startup.
"$objdump" -p "$gui" | grep 'Entry: ID: 0x000018,'
strings "$gui" | grep -F 'name="Microsoft.Windows.Common-Controls"'
strings "$gui" | grep -F 'version="6.0.0.0"'
