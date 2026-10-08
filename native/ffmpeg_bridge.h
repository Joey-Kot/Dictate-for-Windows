#ifndef DICTATE_FFMPEG_BRIDGE_H
#define DICTATE_FFMPEG_BRIDGE_H

#include <stdint.h>
#include <stddef.h>

typedef struct DictateAudioInterval { int64_t start_frame; int64_t end_frame; } DictateAudioInterval;
typedef int (*DictateCancel)(void *);
typedef int (*DictateSamples)(void *, const int16_t *, int);
/* Return nonzero if the host consumed the line; otherwise keep stderr output. */
typedef int (*DictateLog)(const char *);

#ifdef __cplusplus
extern "C" {
#endif

/* Install once, before starting conversions. The callback has process lifetime. */
void dictate_ffmpeg_set_log_callback(DictateLog callback);

/* With samples != NULL, stream mono 16 kHz PCM to the callback and do not open
 * an output. source_rate/source_frames report the actual decoded source domain.
 * Otherwise select source-frame intervals before resampling/encoding.
 * Callbacks and interval storage must remain alive until this function returns.
 * A nonzero callback result aborts processing. Failed outputs are removed. */
int dictate_ffmpeg_convert(
    const char *in_path,
    const char *out_path,
    const char *codec_name,
    int channels,
    int sample_rate,
    int bitrate_kbps,
    int codec_has_bitrate,
    const char *sample_fmt_name,
    int debug,
    const DictateAudioInterval *intervals, size_t interval_count, int intervals_enabled,
    DictateCancel cancel, void *cancel_context,
    DictateSamples samples, void *samples_context,
    int *source_rate, int64_t *source_frames,
    char *errbuf,
    int errbuf_size
);

/* Analyze decoded source frames without modifying the source.  Audio is
 * converted to mono double at its original rate, then silence is decided from
 * each sample's absolute amplitude; the returned intervals use the original
 * per-channel frame timeline and are half-open.  `min_pause_ms` is clamped to
 * at least 1 ms.  The function owns the returned allocation until
 * dictate_ffmpeg_free_intervals is called. */
int dictate_ffmpeg_analyze_silence(
    const char *in_path,
    int64_t min_pause_ms,
    int debug,
    DictateCancel cancel, void *cancel_context,
    DictateAudioInterval **intervals, size_t *interval_count,
    int *source_rate, int64_t *source_frames,
    char *errbuf,
    int errbuf_size
);

void dictate_ffmpeg_free_intervals(DictateAudioInterval *intervals);

/* Export one complete, independently encoded media file per continuous
 * source-frame interval.  Intervals must be an exact contiguous partition of
 * [0, expected_source_frames); malformed plans and source-rate/frame mismatch
 * are rejected.  On failure or cancellation, every output touched by this
 * batch is removed. */
int dictate_ffmpeg_export_segments(
    const char *in_path,
    const char *const *out_paths,
    const DictateAudioInterval *intervals,
    size_t interval_count,
    const char *codec_name,
    int channels,
    int sample_rate,
    int bitrate_kbps,
    int codec_has_bitrate,
    const char *sample_fmt_name,
    int expected_source_rate,
    int64_t expected_source_frames,
    int debug,
    DictateCancel cancel, void *cancel_context,
    char *errbuf,
    int errbuf_size
);

#ifdef __cplusplus
}
#endif

#endif
