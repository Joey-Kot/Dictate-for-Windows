#include "ffmpeg_bridge.h"

#include <errno.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
#include <libavutil/audio_fifo.h>
#include <libavutil/avstring.h>
#include <libavutil/channel_layout.h>
#include <libavutil/error.h>
#include <libavutil/frame.h>
#include <libavutil/log.h>
#include <libavutil/mathematics.h>
#include <libavutil/opt.h>
#include <libavutil/samplefmt.h>
#include <libswresample/swresample.h>

static SttLog stt_log_sink = NULL;

static void stt_log_callback(void *context, int level, const char *format, va_list args) {
    /* The Rust bridge serializes conversions because libav's log level is global.
     * Disabled FFmpeg debug still preserves native errors on stderr. */
    int limit = av_log_get_level();
    if (stt_log_sink != NULL && limit >= AV_LOG_INFO && level <= limit) {
        char line[8192];
        int prefix = 1;
        va_list copy;
        va_copy(copy, args);
        av_log_format_line2(context, level, format, copy, line, sizeof(line), &prefix);
        va_end(copy);
        line[sizeof(line) - 1] = '\0';
        if (stt_log_sink(line)) return;
    }
    av_log_default_callback(context, level, format, args);
}

void stt_ffmpeg_set_log_callback(SttLog callback) {
    stt_log_sink = callback;
    av_log_set_callback(stt_log_callback);
}

static void stt_set_error(char *errbuf, int errbuf_size, const char *fmt, ...) {
    if (errbuf == NULL || errbuf_size <= 0) {
        return;
    }
    va_list args;
    va_start(args, fmt);
    vsnprintf(errbuf, errbuf_size, fmt, args);
    va_end(args);
}

static void stt_set_av_error(char *errbuf, int errbuf_size, const char *prefix, int err) {
    char av_error[AV_ERROR_MAX_STRING_SIZE] = {0};
    av_strerror(err, av_error, sizeof(av_error));
    stt_set_error(errbuf, errbuf_size, "%s: %s", prefix, av_error);
}

static int stt_pick_sample_fmt(const AVCodec *codec, const char *requested) {
    enum AVSampleFormat fmt = AV_SAMPLE_FMT_NONE;
    if (requested != NULL && requested[0] != '\0') {
        fmt = av_get_sample_fmt(requested);
    }
    if (fmt != AV_SAMPLE_FMT_NONE && codec->sample_fmts != NULL) {
        const enum AVSampleFormat *p = codec->sample_fmts;
        while (*p != AV_SAMPLE_FMT_NONE) {
            if (*p == fmt) {
                return fmt;
            }
            p++;
        }
    }
    if (fmt != AV_SAMPLE_FMT_NONE && codec->sample_fmts == NULL) {
        return fmt;
    }
    if (codec->sample_fmts != NULL) {
        return codec->sample_fmts[0];
    }
    return AV_SAMPLE_FMT_S16;
}

static int stt_alloc_audio_frame(
    AVFrame **frame,
    enum AVSampleFormat sample_fmt,
    const AVChannelLayout *ch_layout,
    int sample_rate,
    int nb_samples,
    char *errbuf,
    int errbuf_size
) {
    int ret;
    AVFrame *f = av_frame_alloc();
    if (f == NULL) {
        stt_set_error(errbuf, errbuf_size, "could not allocate audio frame");
        return AVERROR(ENOMEM);
    }
    f->format = sample_fmt;
    f->sample_rate = sample_rate;
    f->nb_samples = nb_samples;
    ret = av_channel_layout_copy(&f->ch_layout, ch_layout);
    if (ret < 0) {
        av_frame_free(&f);
        stt_set_av_error(errbuf, errbuf_size, "could not copy channel layout", ret);
        return ret;
    }
    if (nb_samples > 0) {
        ret = av_frame_get_buffer(f, 0);
        if (ret < 0) {
            av_frame_free(&f);
            stt_set_av_error(errbuf, errbuf_size, "could not allocate audio frame buffer", ret);
            return ret;
        }
    }
    *frame = f;
    return 0;
}

static int stt_encode_write(
    AVCodecContext *enc_ctx,
    AVFormatContext *ofmt_ctx,
    AVStream *out_stream,
    AVFrame *frame,
    char *errbuf,
    int errbuf_size
) {
    int ret = avcodec_send_frame(enc_ctx, frame);
    if (ret < 0) {
        stt_set_av_error(errbuf, errbuf_size, "could not send frame to encoder", ret);
        return ret;
    }

    while (1) {
        AVPacket *pkt = av_packet_alloc();
        if (pkt == NULL) {
            stt_set_error(errbuf, errbuf_size, "could not allocate encoded packet");
            return AVERROR(ENOMEM);
        }
        ret = avcodec_receive_packet(enc_ctx, pkt);
        if (ret == AVERROR(EAGAIN) || ret == AVERROR_EOF) {
            av_packet_free(&pkt);
            return 0;
        }
        if (ret < 0) {
            av_packet_free(&pkt);
            stt_set_av_error(errbuf, errbuf_size, "could not receive encoded packet", ret);
            return ret;
        }
        av_packet_rescale_ts(pkt, enc_ctx->time_base, out_stream->time_base);
        pkt->stream_index = out_stream->index;
        ret = av_interleaved_write_frame(ofmt_ctx, pkt);
        av_packet_free(&pkt);
        if (ret < 0) {
            stt_set_av_error(errbuf, errbuf_size, "could not write encoded packet", ret);
            return ret;
        }
    }
}

static int stt_write_fifo_to_encoder(
    AVAudioFifo *fifo,
    AVCodecContext *enc_ctx,
    AVFormatContext *ofmt_ctx,
    AVStream *out_stream,
    int flush_all,
    int64_t *next_pts,
    char *errbuf,
    int errbuf_size
) {
    int ret = 0;
    while (av_audio_fifo_size(fifo) > 0) {
        int available = av_audio_fifo_size(fifo);
        int nb_samples = enc_ctx->frame_size > 0 ? enc_ctx->frame_size : available;
        if (!flush_all && available < nb_samples) {
            return 0;
        }
        if (flush_all && available < nb_samples) {
            nb_samples = available;
        }

        AVFrame *frame = NULL;
        ret = stt_alloc_audio_frame(
            &frame,
            enc_ctx->sample_fmt,
            &enc_ctx->ch_layout,
            enc_ctx->sample_rate,
            nb_samples,
            errbuf,
            errbuf_size
        );
        if (ret < 0) {
            return ret;
        }
        ret = av_audio_fifo_read(fifo, (void **)frame->extended_data, nb_samples);
        if (ret < nb_samples) {
            av_frame_free(&frame);
            stt_set_error(errbuf, errbuf_size, "could not read converted samples from fifo");
            return AVERROR(EIO);
        }
        frame->pts = *next_pts;
        *next_pts += frame->nb_samples;
        ret = stt_encode_write(enc_ctx, ofmt_ctx, out_stream, frame, errbuf, errbuf_size);
        av_frame_free(&frame);
        if (ret < 0) {
            return ret;
        }
    }
    return 0;
}

static int stt_convert_and_queue_frame(
    SwrContext *swr,
    const AVFrame *input_format,
    AVCodecContext *enc_ctx,
    AVAudioFifo *fifo,
    AVFrame *decoded,
    char *errbuf,
    int errbuf_size
) {
    int64_t delay = swr_get_delay(swr, input_format->sample_rate);
    int out_samples = (int)av_rescale_rnd(
        delay + decoded->nb_samples,
        enc_ctx->sample_rate,
        input_format->sample_rate,
        AV_ROUND_UP
    );
    if (out_samples <= 0) {
        return 0;
    }

    AVFrame *converted = NULL;
    int ret = stt_alloc_audio_frame(
        &converted,
        enc_ctx->sample_fmt,
        &enc_ctx->ch_layout,
        enc_ctx->sample_rate,
        out_samples,
        errbuf,
        errbuf_size
    );
    if (ret < 0) {
        return ret;
    }
    ret = swr_convert(
        swr,
        converted->extended_data,
        out_samples,
        (const uint8_t **)decoded->extended_data,
        decoded->nb_samples
    );
    if (ret < 0) {
        av_frame_free(&converted);
        stt_set_av_error(errbuf, errbuf_size, "could not resample audio frame", ret);
        return ret;
    }
    converted->nb_samples = ret;
    if (ret > 0) {
        ret = av_audio_fifo_realloc(fifo, av_audio_fifo_size(fifo) + converted->nb_samples);
        if (ret < 0) {
            av_frame_free(&converted);
            stt_set_av_error(errbuf, errbuf_size, "could not grow audio fifo", ret);
            return ret;
        }
        ret = av_audio_fifo_write(fifo, (void **)converted->extended_data, converted->nb_samples);
        if (ret < converted->nb_samples) {
            av_frame_free(&converted);
            stt_set_error(errbuf, errbuf_size, "could not write converted samples to fifo");
            return AVERROR(EIO);
        }
    }
    av_frame_free(&converted);
    return 0;
}

static int stt_flush_resampler(
    SwrContext *swr,
    const AVFrame *input_format,
    AVCodecContext *enc_ctx,
    AVAudioFifo *fifo,
    char *errbuf,
    int errbuf_size
) {
    while (1) {
        int64_t delay = swr_get_delay(swr, input_format->sample_rate);
        int out_samples = (int)av_rescale_rnd(
            delay,
            enc_ctx->sample_rate,
            input_format->sample_rate,
            AV_ROUND_UP
        );
        if (out_samples <= 0) {
            return 0;
        }
        AVFrame *converted = NULL;
        int ret = stt_alloc_audio_frame(
            &converted,
            enc_ctx->sample_fmt,
            &enc_ctx->ch_layout,
            enc_ctx->sample_rate,
            out_samples,
            errbuf,
            errbuf_size
        );
        if (ret < 0) {
            return ret;
        }
        ret = swr_convert(swr, converted->extended_data, out_samples, NULL, 0);
        if (ret < 0) {
            av_frame_free(&converted);
            stt_set_av_error(errbuf, errbuf_size, "could not flush resampler", ret);
            return ret;
        }
        converted->nb_samples = ret;
        if (ret == 0) {
            av_frame_free(&converted);
            return 0;
        }
        ret = av_audio_fifo_realloc(fifo, av_audio_fifo_size(fifo) + converted->nb_samples);
        if (ret < 0) {
            av_frame_free(&converted);
            stt_set_av_error(errbuf, errbuf_size, "could not grow audio fifo", ret);
            return ret;
        }
        ret = av_audio_fifo_write(fifo, (void **)converted->extended_data, converted->nb_samples);
        if (ret < converted->nb_samples) {
            av_frame_free(&converted);
            stt_set_error(errbuf, errbuf_size, "could not write flushed samples to fifo");
            return AVERROR(EIO);
        }
        av_frame_free(&converted);
    }
}


typedef struct SttGate {
    const SttAudioInterval *intervals;
    size_t count, index;
    int enabled;
    int64_t position;
    SttCancel cancel;
    void *context;
} SttGate;

static int stt_interrupt(void *opaque) {
    SttGate *gate = opaque;
    return gate->cancel && gate->cancel(gate->context);
}

static int stt_gate_frame(SttGate *gate, SwrContext *swr, const AVFrame *input_format,
    AVCodecContext *enc, AVAudioFifo *fifo, AVFrame *frame, char *errbuf, int errbuf_size) {
    if (stt_interrupt(gate)) return AVERROR_EXIT;
    if (frame->sample_rate != input_format->sample_rate ||
        frame->format != input_format->format ||
        av_channel_layout_compare(&frame->ch_layout, &input_format->ch_layout)) {
        stt_set_error(errbuf, errbuf_size, "input audio format changed during decoding");
        return AVERROR_INVALIDDATA;
    }
    if (frame->nb_samples > INT64_MAX - gate->position) return AVERROR(EOVERFLOW);
    int64_t start = gate->position, end = start + frame->nb_samples;
    gate->position = end;
    if (!gate->enabled)
        return stt_convert_and_queue_frame(swr, input_format, enc, fifo, frame, errbuf, errbuf_size);
    while (gate->index < gate->count) {
        if (stt_interrupt(gate)) return AVERROR_EXIT;
        const SttAudioInterval *i = &gate->intervals[gate->index];
        if (i->end_frame <= start) { gate->index++; continue; }
        if (i->start_frame >= end) break;
        int64_t left = FFMAX(start, i->start_frame), right = FFMIN(end, i->end_frame);
        AVFrame *selected = NULL;
        int ret = stt_alloc_audio_frame(&selected, frame->format, &frame->ch_layout,
            frame->sample_rate, (int)(right-left), errbuf, errbuf_size);
        if (ret < 0) return ret;
        ret = av_samples_copy(selected->extended_data, frame->extended_data, 0,
            (int)(left-start), (int)(right-left), frame->ch_layout.nb_channels, frame->format);
        if (ret >= 0)
            ret = stt_convert_and_queue_frame(swr, input_format, enc, fifo, selected, errbuf, errbuf_size);
        av_frame_free(&selected);
        if (ret < 0) return ret;
        if (i->end_frame <= end) gate->index++;
        else break;
    }
    return 0;
}

static int stt_deliver_samples(AVAudioFifo *fifo, SttSamples callback, void *context) {
    int16_t buffer[4096];
    while (av_audio_fifo_size(fifo) > 0) {
        int n = FFMIN(av_audio_fifo_size(fifo), 4096);
        void *planes[] = {buffer};
        if (av_audio_fifo_read(fifo, planes, n) != n) return AVERROR(EIO);
        if (callback(context, buffer, n)) return AVERROR_EXTERNAL;
    }
    return 0;
}

int stt_ffmpeg_convert(
    const char *in_path,
    const char *out_path,
    const char *codec_name,
    int channels,
    int sample_rate,
    int bitrate_kbps,
    int codec_has_bitrate,
    const char *sample_fmt_name,
    int debug,
    const SttAudioInterval *intervals, size_t interval_count, int intervals_enabled,
    SttCancel cancel, void *cancel_context,
    SttSamples samples, void *samples_context,
    int *source_rate, int64_t *source_frames,
    char *errbuf,
    int errbuf_size
) {
    AVFormatContext *ifmt_ctx = NULL;
    AVFormatContext *ofmt_ctx = NULL;
    AVCodecContext *dec_ctx = NULL;
    AVCodecContext *enc_ctx = NULL;
    AVPacket *packet = NULL;
    AVFrame *decoded = NULL;
    AVFrame *input_format = NULL;
    AVAudioFifo *fifo = NULL;
    SwrContext *swr = NULL;
    AVStream *out_stream = NULL;
    int audio_stream = -1;
    int ret = 0;
    int64_t next_pts = 0;
    int output_opened = 0;
    SttGate gate = {intervals, interval_count, 0, intervals_enabled, 0, cancel, cancel_context};
    if (intervals_enabled && (!interval_count || !intervals)) return AVERROR(EINVAL);
    for (size_t n = 0; n < interval_count; n++) {
        if (intervals[n].start_frame < 0 || intervals[n].end_frame <= intervals[n].start_frame ||
            (n && intervals[n].start_frame < intervals[n-1].end_frame)) return AVERROR(EINVAL);
    }
    ifmt_ctx = avformat_alloc_context();
    if (!ifmt_ctx) return AVERROR(ENOMEM);
    ifmt_ctx->interrupt_callback = (AVIOInterruptCB){stt_interrupt, &gate};

    av_log_set_level(debug ? AV_LOG_INFO : AV_LOG_ERROR);

    ret = avformat_open_input(&ifmt_ctx, in_path, NULL, NULL);
    if (ret < 0) {
        stt_set_av_error(errbuf, errbuf_size, "could not open input audio", ret);
        goto cleanup;
    }
    ret = avformat_find_stream_info(ifmt_ctx, NULL);
    if (ret < 0) {
        stt_set_av_error(errbuf, errbuf_size, "could not read input stream info", ret);
        goto cleanup;
    }
    audio_stream = av_find_best_stream(ifmt_ctx, AVMEDIA_TYPE_AUDIO, -1, -1, NULL, 0);
    if (audio_stream < 0) {
        ret = audio_stream;
        stt_set_av_error(errbuf, errbuf_size, "could not find input audio stream", ret);
        goto cleanup;
    }

    AVStream *in_stream = ifmt_ctx->streams[audio_stream];
    const AVCodec *decoder = avcodec_find_decoder(in_stream->codecpar->codec_id);
    if (decoder == NULL) {
        ret = AVERROR_DECODER_NOT_FOUND;
        stt_set_error(errbuf, errbuf_size, "could not find decoder for input audio");
        goto cleanup;
    }
    dec_ctx = avcodec_alloc_context3(decoder);
    if (dec_ctx == NULL) {
        ret = AVERROR(ENOMEM);
        stt_set_error(errbuf, errbuf_size, "could not allocate decoder context");
        goto cleanup;
    }
    ret = avcodec_parameters_to_context(dec_ctx, in_stream->codecpar);
    if (ret < 0) {
        stt_set_av_error(errbuf, errbuf_size, "could not copy decoder parameters", ret);
        goto cleanup;
    }
    if (dec_ctx->ch_layout.nb_channels <= 0) {
        int input_channels = in_stream->codecpar->ch_layout.nb_channels;
        if (input_channels <= 0) {
            input_channels = channels;
        }
        av_channel_layout_default(&dec_ctx->ch_layout, input_channels);
    }
    ret = avcodec_open2(dec_ctx, decoder, NULL);
    if (ret < 0) {
        stt_set_av_error(errbuf, errbuf_size, "could not open decoder", ret);
        goto cleanup;
    }

    if (samples) {
        enc_ctx = avcodec_alloc_context3(NULL);
        if (!enc_ctx) { ret = AVERROR(ENOMEM); goto cleanup; }
        enc_ctx->sample_rate = 16000;
        enc_ctx->sample_fmt = AV_SAMPLE_FMT_S16;
        av_channel_layout_default(&enc_ctx->ch_layout, 1);
    } else {
        // Raw PCM muxers have no matching conventional filename extension.
        // Use their runtime format names only for the explicit raw extensions.
        const char *output_format = NULL;
        const char *extension = strrchr(out_path, '.');
        static const char *const raw_formats[] = {
            "s8", "s16le", "s16be", "s24le", "s24be", "s32le", "s32be",
            "f32le", "f32be", "f64le", "f64be", "alaw", "mulaw"
        };
        if (extension != NULL) {
            if (av_strcasecmp(extension + 1, "mka") == 0) {
                output_format = "matroska";
            }
            for (size_t i = 0; i < sizeof(raw_formats) / sizeof(raw_formats[0]); i++) {
                if (av_strcasecmp(extension + 1, raw_formats[i]) == 0) {
                    output_format = raw_formats[i];
                    break;
                }
            }
        }
        ret = avformat_alloc_output_context2(&ofmt_ctx, NULL, output_format, out_path);
        if (ret < 0 || ofmt_ctx == NULL) {
            if (ret >= 0) ret = AVERROR(ENOMEM);
            stt_set_av_error(errbuf, errbuf_size, "could not create output container", ret);
            goto cleanup;
        }
        const AVCodec *encoder = avcodec_find_encoder_by_name(codec_name);
        if (encoder == NULL) {
            ret = AVERROR_ENCODER_NOT_FOUND;
            stt_set_error(errbuf, errbuf_size, "could not find encoder '%s'", codec_name);
            goto cleanup;
        }
        enc_ctx = avcodec_alloc_context3(encoder);
        if (enc_ctx == NULL) {
            ret = AVERROR(ENOMEM);
            stt_set_error(errbuf, errbuf_size, "could not allocate encoder context");
            goto cleanup;
        }
        enc_ctx->sample_rate = sample_rate;
        enc_ctx->sample_fmt = stt_pick_sample_fmt(encoder, sample_fmt_name);
        enc_ctx->time_base = (AVRational){1, sample_rate};
        if (codec_has_bitrate) {
            enc_ctx->bit_rate = (int64_t)bitrate_kbps * 1000;
        }
        av_channel_layout_default(&enc_ctx->ch_layout, channels);
        if (ofmt_ctx->oformat->flags & AVFMT_GLOBALHEADER) {
            enc_ctx->flags |= AV_CODEC_FLAG_GLOBAL_HEADER;
        }
        ret = avcodec_open2(enc_ctx, encoder, NULL);
        if (ret < 0) {
            stt_set_av_error(errbuf, errbuf_size, "could not open encoder", ret);
            goto cleanup;
        }

        out_stream = avformat_new_stream(ofmt_ctx, NULL);
        if (out_stream == NULL) {
            ret = AVERROR(ENOMEM);
            stt_set_error(errbuf, errbuf_size, "could not allocate output stream");
            goto cleanup;
        }
        out_stream->time_base = enc_ctx->time_base;
        ret = avcodec_parameters_from_context(out_stream->codecpar, enc_ctx);
        if (ret < 0) {
            stt_set_av_error(errbuf, errbuf_size, "could not copy encoder parameters", ret);
            goto cleanup;
        }

    }
    // Decoder contexts can change while decoding. Keep an owned snapshot of
    // the resampler's input format for validation and source-frame accounting.
    input_format = av_frame_alloc();
    if (!input_format) { ret = AVERROR(ENOMEM); goto cleanup; }
    input_format->sample_rate = dec_ctx->sample_rate;
    input_format->format = dec_ctx->sample_fmt;
    ret = av_channel_layout_copy(&input_format->ch_layout, &dec_ctx->ch_layout);
    if (ret < 0) {
        stt_set_av_error(errbuf, errbuf_size, "could not copy input channel layout", ret);
        goto cleanup;
    }
    ret = swr_alloc_set_opts2(
        &swr,
        &enc_ctx->ch_layout,
        enc_ctx->sample_fmt,
        enc_ctx->sample_rate,
        &input_format->ch_layout,
        input_format->format,
        input_format->sample_rate,
        0,
        NULL
    );
    if (ret < 0) {
        stt_set_av_error(errbuf, errbuf_size, "could not allocate resampler", ret);
        goto cleanup;
    }
    ret = swr_init(swr);
    if (ret < 0) {
        stt_set_av_error(errbuf, errbuf_size, "could not initialize resampler", ret);
        goto cleanup;
    }

    fifo = av_audio_fifo_alloc(
        enc_ctx->sample_fmt,
        enc_ctx->ch_layout.nb_channels,
        enc_ctx->frame_size > 0 ? enc_ctx->frame_size : 1024
    );
    if (fifo == NULL) {
        ret = AVERROR(ENOMEM);
        stt_set_error(errbuf, errbuf_size, "could not allocate audio fifo");
        goto cleanup;
    }

    if (!samples) {
        ofmt_ctx->interrupt_callback = (AVIOInterruptCB){stt_interrupt, &gate};
        if (!(ofmt_ctx->oformat->flags & AVFMT_NOFILE)) {
            ret = avio_open2(&ofmt_ctx->pb, out_path, AVIO_FLAG_WRITE, &ofmt_ctx->interrupt_callback, NULL);
            output_opened = ret >= 0;
            if (ret < 0) {
                stt_set_av_error(errbuf, errbuf_size, "could not open output audio", ret);
                goto cleanup;
            }
        }
        ret = avformat_write_header(ofmt_ctx, NULL);
        if (ret < 0) {
            stt_set_av_error(errbuf, errbuf_size, "could not write output header", ret);
            goto cleanup;
        }

    }
    packet = av_packet_alloc();
    decoded = av_frame_alloc();
    if (packet == NULL || decoded == NULL) {
        ret = AVERROR(ENOMEM);
        stt_set_error(errbuf, errbuf_size, "could not allocate decode buffers");
        goto cleanup;
    }

    while ((ret = av_read_frame(ifmt_ctx, packet)) >= 0) {
        if (stt_interrupt(&gate)) { ret = AVERROR_EXIT; goto cleanup; }
        if (packet->stream_index != audio_stream) {
            av_packet_unref(packet);
            continue;
        }
        ret = avcodec_send_packet(dec_ctx, packet);
        av_packet_unref(packet);
        if (ret < 0) {
            stt_set_av_error(errbuf, errbuf_size, "could not send packet to decoder", ret);
            goto cleanup;
        }
        while ((ret = avcodec_receive_frame(dec_ctx, decoded)) >= 0) {
            ret = stt_gate_frame(&gate,
                swr,
                input_format,
                enc_ctx,
                fifo,
                decoded,
                errbuf,
                errbuf_size
            );
            av_frame_unref(decoded);
            if (ret < 0) {
                goto cleanup;
            }
            ret = samples ? stt_deliver_samples(fifo, samples, samples_context) : stt_write_fifo_to_encoder(
                fifo,
                enc_ctx,
                ofmt_ctx,
                out_stream,
                0,
                &next_pts,
                errbuf,
                errbuf_size
            );
            if (ret < 0) {
                goto cleanup;
            }
        }
        if (ret != AVERROR(EAGAIN) && ret != AVERROR_EOF) {
            stt_set_av_error(errbuf, errbuf_size, "could not receive decoded frame", ret);
            goto cleanup;
        }
    }
    if (ret != AVERROR_EOF) {
        stt_set_av_error(errbuf, errbuf_size, "could not read input packet", ret);
        goto cleanup;
    }

    ret = avcodec_send_packet(dec_ctx, NULL);
    if (ret < 0) {
        stt_set_av_error(errbuf, errbuf_size, "could not flush decoder", ret);
        goto cleanup;
    }
    while ((ret = avcodec_receive_frame(dec_ctx, decoded)) >= 0) {
        ret = stt_gate_frame(&gate,
            swr,
            input_format,
            enc_ctx,
            fifo,
            decoded,
            errbuf,
            errbuf_size
        );
        av_frame_unref(decoded);
        if (ret < 0) {
            goto cleanup;
        }
        ret = samples ? stt_deliver_samples(fifo, samples, samples_context) : stt_write_fifo_to_encoder(
            fifo,
            enc_ctx,
            ofmt_ctx,
            out_stream,
            0,
            &next_pts,
            errbuf,
            errbuf_size
        );
        if (ret < 0) {
            goto cleanup;
        }
    }
    if (ret != AVERROR_EOF && ret != AVERROR(EAGAIN)) {
        stt_set_av_error(errbuf, errbuf_size, "could not receive flushed decoded frame", ret);
        goto cleanup;
    }

    ret = stt_flush_resampler(swr, input_format, enc_ctx, fifo, errbuf, errbuf_size);
    if (ret < 0) {
        goto cleanup;
    }
    ret = samples ? stt_deliver_samples(fifo, samples, samples_context) : stt_write_fifo_to_encoder(
        fifo,
        enc_ctx,
        ofmt_ctx,
        out_stream,
        1,
        &next_pts,
        errbuf,
        errbuf_size
    );
    if (ret < 0) {
        goto cleanup;
    }
    if (!samples) {
        ret = stt_encode_write(enc_ctx, ofmt_ctx, out_stream, NULL, errbuf, errbuf_size);
        if (ret < 0) {
            goto cleanup;
        }
        ret = av_write_trailer(ofmt_ctx);
        if (ret < 0) {
            stt_set_av_error(errbuf, errbuf_size, "could not write output trailer", ret);
            goto cleanup;
        }
        ret = 0;

    }
    if (intervals_enabled && gate.position < intervals[interval_count-1].end_frame) {
        ret = AVERROR_INVALIDDATA;
        stt_set_error(errbuf, errbuf_size, "input ended before selected intervals");
    }
    if (source_rate) *source_rate = input_format->sample_rate;
    if (source_frames) *source_frames = gate.position;

cleanup:
    av_frame_free(&input_format);
    if (decoded != NULL) {
        av_frame_free(&decoded);
    }
    if (packet != NULL) {
        av_packet_free(&packet);
    }
    if (fifo != NULL) {
        av_audio_fifo_free(fifo);
    }
    if (swr != NULL) {
        swr_free(&swr);
    }
    if (enc_ctx != NULL) {
        avcodec_free_context(&enc_ctx);
    }
    if (dec_ctx != NULL) {
        avcodec_free_context(&dec_ctx);
    }
    if (ofmt_ctx != NULL) {
        if (!(ofmt_ctx->oformat->flags & AVFMT_NOFILE) && ofmt_ctx->pb != NULL) {
            int close_ret = avio_closep(&ofmt_ctx->pb);
            if (ret >= 0 && close_ret < 0) ret = close_ret;
        }
        avformat_free_context(ofmt_ctx);
    }
    if (ifmt_ctx != NULL) {
        avformat_close_input(&ifmt_ctx);
    }
    if (stt_interrupt(&gate)) ret = AVERROR_EXIT;
    if (ret < 0 && output_opened) remove(out_path);
    return ret;
}
