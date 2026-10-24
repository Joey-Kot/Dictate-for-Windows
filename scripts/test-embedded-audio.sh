#!/usr/bin/env bash
# Linux native validation using the same FFmpeg source as the Windows build.
# Requires gcc, make, pkg-config, FFmpeg CLI with Flite, and development headers
# for libopus, libmp3lame and libvorbis. Nothing is installed globally.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ffmpeg_version=9.0.2
ffmpeg_sha256=84960df915059e8754fef2cd7c9afeb614062b1b5458ec471eecee619ee04e98
task_dir="${DICTATE_NATIVE_TEST_DIR:-$root/native-audio-test-tmp/ffmpeg-$ffmpeg_version}"
source_archive="${DICTATE_FFMPEG_ARCHIVE:-$root/build/ffmpeg/ffmpeg-$ffmpeg_version.tar.gz}"
printf '%s  %s\n' "$ffmpeg_sha256" "$source_archive" | sha256sum --check --strict
mkdir -p "$task_dir/source" "$task_dir/build"
tar -xf "$source_archive" --strip-components=1 -C "$task_dir/source"
cd "$task_dir/build"
../source/configure \
    --prefix="$task_dir/install" --disable-programs --disable-doc --disable-debug \
    --disable-autodetect --disable-everything --disable-x86asm --disable-avfilter \
    --enable-avcodec --enable-avformat --enable-avutil --enable-swresample \
    --enable-protocol=file \
    --enable-demuxer=wav,mp3,flac,ogg,mov,aac,matroska,wv,ac3,eac3 \
    --enable-parser=mpegaudio,flac,opus,vorbis,aac,aac_latm,ac3 \
    --enable-decoder=mp3,mp3float,mp2,flac,opus,vorbis,aac,aac_latm,alac,wavpack,ac3,eac3,pcm_u8,pcm_s16le,pcm_s24le,pcm_s32le,pcm_f32le \
    --enable-encoder=pcm_s16le,pcm_s24le,pcm_f32le,flac,aac,ac3,eac3,alac,wavpack,libopus,libmp3lame,libvorbis \
    --enable-muxer=wav,flac,adts,ipod,mp4,ac3,eac3,wv,ogg,mp3,webm \
    --enable-libopus --enable-libmp3lame --enable-libvorbis
make -j"${JOBS:-4}"
make install
cd "$root"
bash scripts/generate-audio-test-fixtures.sh "$task_dir/fixtures"
export PKG_CONFIG_PATH="$task_dir/install/lib/pkgconfig"
export DICTATE_AUDIO_FIXTURES="$task_dir/fixtures"
export CARGO_TARGET_DIR="$task_dir/target"
cargo test --workspace --features dictate-core/static-libav
cargo clippy --workspace --all-targets --features dictate-core/static-libav -- -D warnings
