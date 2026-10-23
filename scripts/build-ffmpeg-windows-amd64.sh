#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD_DIR="${BUILD_DIR:-$ROOT_DIR/build/ffmpeg}"
PREFIX="${PREFIX:-$ROOT_DIR/build/ffmpeg-win}"
TARGET="${TARGET:-x86_64-w64-mingw32}"
JOBS="${JOBS:-$(nproc)}"

OGG_VERSION="${OGG_VERSION:-1.3.5}"
VORBIS_VERSION="${VORBIS_VERSION:-1.3.7}"
OPUS_VERSION="1.5.2"
OPUS_ARCHIVE="opus-$OPUS_VERSION.tar.gz"
OPUS_URL="https://downloads.xiph.org/releases/opus/$OPUS_ARCHIVE"
OPUS_SHA256="65c1d2f78b9f2fb20082c38cbe47c951ad5839345876e46941612ee87f9a7ce1"
LAME_VERSION="3.100"
LAME_ARCHIVE="lame-$LAME_VERSION.tar.gz"
LAME_URL="https://downloads.sourceforge.net/project/lame/lame/$LAME_VERSION/$LAME_ARCHIVE"
LAME_SHA256="ddfe36cab873794038ae2c1210557ad34857a4b6bdc515785d1da9e175b1da1e"
OPENCORE_AMR_VERSION="${OPENCORE_AMR_VERSION:-0.1.6}"
SPEEX_VERSION="${SPEEX_VERSION:-1.2.1}"
VO_AMRWBENC_VERSION="${VO_AMRWBENC_VERSION:-0.1.3}"
FFMPEG_VERSION="8.1.3"
FFMPEG_ARCHIVE="ffmpeg-$FFMPEG_VERSION.tar.xz"
FFMPEG_URL="https://ffmpeg.org/releases/$FFMPEG_ARCHIVE"
FFMPEG_SHA256="7138d28c96d9d3e3af4ee3d8cad72741f8ffb40da90c1112235dea3ecd3178a3"

mkdir -p "$BUILD_DIR" "$PREFIX"

export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig"
export PKG_CONFIG_LIBDIR="$PREFIX/lib/pkgconfig"
export CFLAGS="-O2 -I$PREFIX/include"
export LDFLAGS="-L$PREFIX/lib"

fetch_tarball() {
	local url="$1"
	local out="$2"
	if [ ! -f "$out" ]; then
		curl --fail --location "$url" -o "$out.part"
		mv "$out.part" "$out"
	fi
}

extract_once() {
	local tarball="$1"
	local dir="$2"
	if [ ! -d "$dir" ]; then
		tar -xf "$tarball" -C "$BUILD_DIR"
	fi
}

build_autotools() {
	local dir="$1"
	shift
	cd "$dir"
	if [ ! -x ./configure ]; then
		if [ -x ./autogen.sh ]; then
			./autogen.sh
		else
			autoreconf -fiv
		fi
	fi
	./configure \
		--host="$TARGET" \
		--prefix="$PREFIX" \
		--disable-shared \
		--enable-static \
		"$@"
	make -j"$JOBS"
	make install
}

cd "$BUILD_DIR"

fetch_tarball "https://downloads.xiph.org/releases/ogg/libogg-$OGG_VERSION.tar.xz" "$BUILD_DIR/libogg-$OGG_VERSION.tar.xz"
extract_once "$BUILD_DIR/libogg-$OGG_VERSION.tar.xz" "$BUILD_DIR/libogg-$OGG_VERSION"
build_autotools "$BUILD_DIR/libogg-$OGG_VERSION"

fetch_tarball "https://downloads.xiph.org/releases/vorbis/libvorbis-$VORBIS_VERSION.tar.xz" "$BUILD_DIR/libvorbis-$VORBIS_VERSION.tar.xz"
extract_once "$BUILD_DIR/libvorbis-$VORBIS_VERSION.tar.xz" "$BUILD_DIR/libvorbis-$VORBIS_VERSION"
build_autotools "$BUILD_DIR/libvorbis-$VORBIS_VERSION" --disable-oggtest

fetch_tarball "$OPUS_URL" "$BUILD_DIR/$OPUS_ARCHIVE"
printf '%s  %s\n' "$OPUS_SHA256" "$BUILD_DIR/$OPUS_ARCHIVE" | sha256sum --check --strict
extract_once "$BUILD_DIR/$OPUS_ARCHIVE" "$BUILD_DIR/opus-$OPUS_VERSION"
build_autotools "$BUILD_DIR/opus-$OPUS_VERSION" --disable-extra-programs --disable-doc

fetch_tarball "$LAME_URL" "$BUILD_DIR/$LAME_ARCHIVE"
printf '%s  %s\n' "$LAME_SHA256" "$BUILD_DIR/$LAME_ARCHIVE" | sha256sum --check --strict
extract_once "$BUILD_DIR/$LAME_ARCHIVE" "$BUILD_DIR/lame-$LAME_VERSION"
build_autotools "$BUILD_DIR/lame-$LAME_VERSION" --disable-frontend --disable-decoder

fetch_tarball "https://downloads.sourceforge.net/project/opencore-amr/opencore-amr/opencore-amr-$OPENCORE_AMR_VERSION.tar.gz" "$BUILD_DIR/opencore-amr-$OPENCORE_AMR_VERSION.tar.gz"
extract_once "$BUILD_DIR/opencore-amr-$OPENCORE_AMR_VERSION.tar.gz" "$BUILD_DIR/opencore-amr-$OPENCORE_AMR_VERSION"
build_autotools "$BUILD_DIR/opencore-amr-$OPENCORE_AMR_VERSION"

fetch_tarball "https://downloads.xiph.org/releases/speex/speex-$SPEEX_VERSION.tar.gz" "$BUILD_DIR/speex-$SPEEX_VERSION.tar.gz"
extract_once "$BUILD_DIR/speex-$SPEEX_VERSION.tar.gz" "$BUILD_DIR/speex-$SPEEX_VERSION"
build_autotools "$BUILD_DIR/speex-$SPEEX_VERSION" --disable-binaries --disable-examples

fetch_tarball "https://downloads.sourceforge.net/project/opencore-amr/vo-amrwbenc/vo-amrwbenc-$VO_AMRWBENC_VERSION.tar.gz" "$BUILD_DIR/vo-amrwbenc-$VO_AMRWBENC_VERSION.tar.gz"
extract_once "$BUILD_DIR/vo-amrwbenc-$VO_AMRWBENC_VERSION.tar.gz" "$BUILD_DIR/vo-amrwbenc-$VO_AMRWBENC_VERSION"
build_autotools "$BUILD_DIR/vo-amrwbenc-$VO_AMRWBENC_VERSION"

fetch_tarball "$FFMPEG_URL" "$BUILD_DIR/$FFMPEG_ARCHIVE"
printf '%s  %s\n' "$FFMPEG_SHA256" "$BUILD_DIR/$FFMPEG_ARCHIVE" | sha256sum --check --strict
extract_once "$BUILD_DIR/$FFMPEG_ARCHIVE" "$BUILD_DIR/ffmpeg-$FFMPEG_VERSION"
"$ROOT_DIR/scripts/apply-ffmpeg-security-patch.sh" "$BUILD_DIR/ffmpeg-$FFMPEG_VERSION"
cd "$BUILD_DIR/ffmpeg-$FFMPEG_VERSION"

# These are configure component names, not filename extensions or runtime aliases.
# Speex uses Ogg/SPX, WMA uses ASF, M4A uses IPOD, MKV uses MATROSKA,
# AAC uses ADTS, MPEG uses MPEG1SYSTEM, and raw PCM muxers need the pcm_ prefix.
# WMA Pro/Lossless, WMV3 and VC-1 are intentionally excluded.
demuxers=wav,mp3,flac,ogg,mov,aac,matroska,wv,ac3,eac3,amr,avi,flv,mpegps,asf,aiff,pcm_s8,pcm_s16be,pcm_s16le,pcm_s24be,pcm_s24le,pcm_s32be,pcm_s32le,pcm_f32be,pcm_f32le,pcm_f64be,pcm_f64le,pcm_alaw,pcm_mulaw
parsers=mpegaudio,flac,opus,vorbis,aac,aac_latm,ac3,amr
decoders=mp3,mp3float,mp2,flac,opus,vorbis,aac,aac_latm,alac,wavpack,ac3,eac3,amrnb,amrwb,libspeex,wmav1,wmav2,pcm_u8,pcm_alaw,pcm_mulaw,adpcm_ms,adpcm_ima_wav,pcm_f32be,pcm_f32le,pcm_f64be,pcm_f64le,pcm_s16be,pcm_s16le,pcm_s24be,pcm_s24le,pcm_s32be,pcm_s32le,pcm_s64be,pcm_s64le,pcm_s8
encoders=libopus,wavpack,aac,ac3,eac3,libmp3lame,mp2,flac,alac,libvorbis,adpcm_ms,libopencore_amrnb,libvo_amrwbenc,libspeex,wmav1,wmav2,pcm_alaw,pcm_mulaw,pcm_f32be,pcm_f32le,pcm_f64be,pcm_f64le,pcm_s16be,pcm_s16le,pcm_s24be,pcm_s24le,pcm_s32be,pcm_s32le,pcm_s64be,pcm_s64le,pcm_s8
muxers=wav,ac3,ogg,mp3,flac,eac3,adts,ipod,mov,mp4,opus,webm,wv,spx,amr,avi,flv,matroska,mpeg1system,asf,aiff,pcm_s8,pcm_s16be,pcm_s16le,pcm_s24be,pcm_s24le,pcm_s32be,pcm_s32le,pcm_f32be,pcm_f32le,pcm_f64be,pcm_f64le,pcm_alaw,pcm_mulaw

./configure \
	--prefix="$PREFIX" \
	--target-os=mingw32 \
	--arch=x86_64 \
	--cross-prefix="$TARGET-" \
	--pkg-config=pkg-config \
	--pkg-config-flags=--static \
	--enable-cross-compile \
	--disable-shared \
	--enable-static \
	--disable-debug \
	--disable-doc \
	--disable-programs \
	--disable-autodetect \
	--disable-everything \
	--enable-gpl \
	--enable-version3 \
	--enable-avcodec \
	--enable-avformat \
	--enable-avutil \
	--enable-swresample \
	--enable-protocol=file \
	--disable-avfilter \
	--enable-demuxer="$demuxers" \
	--enable-parser="$parsers" \
	--enable-decoder="$decoders" \
	--enable-encoder="$encoders" \
	--enable-muxer="$muxers" \
	--enable-libopus \
	--enable-libmp3lame \
	--enable-libvorbis \
	--enable-libopencore-amrnb \
	--enable-libvo-amrwbenc \
	--enable-libspeex

# configure can succeed after silently ignoring an unknown or unavailable component.
# Fail before compilation if any requested component was not actually enabled.
check_components() {
	local kind="$1" component symbol
	local -a components
	IFS=, read -r -a components <<< "$2"
	for component in "${components[@]}"; do
		symbol="CONFIG_${component^^}_${kind}"
		if ! grep -qx "#define $symbol 1" config_components.h; then
			echo "Required FFmpeg component was not enabled: $component ($kind)" >&2
			exit 1
		fi
	done
}
check_components DEMUXER "$demuxers"
check_components PARSER "$parsers"
check_components DECODER "$decoders"
check_components ENCODER "$encoders"
check_components MUXER "$muxers"

make -j"$JOBS"
make install
