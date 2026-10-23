#!/usr/bin/env bash
# Apply the same audited backport to release builds and native test builds.
set -euo pipefail
source_dir="${1:?Pass the extracted FFmpeg 8.1.3 source directory}"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
patch_file="$script_dir/patches/ffmpeg-8.1.3-cve-2026-107678.patch"

if [[ "$(cat "$source_dir/VERSION")" != "8.1.3" ]]; then
    echo "The security backport requires FFmpeg 8.1.3 sources." >&2
    exit 1
fi

if patch --directory="$source_dir" -p1 --batch --forward --fuzz=0 --dry-run --silent < "$patch_file"; then
    patch --directory="$source_dir" -p1 --batch --forward --fuzz=0 --no-backup-if-mismatch < "$patch_file"
elif patch --directory="$source_dir" -p1 --batch --reverse --fuzz=0 --dry-run --silent < "$patch_file"; then
    echo "FFmpeg CVE-2026-107678 backport is already applied."
else
    echo "FFmpeg sources do not match the audited security backport." >&2
    exit 1
fi
