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

#ifdef __cplusplus
}
#endif

#endif

