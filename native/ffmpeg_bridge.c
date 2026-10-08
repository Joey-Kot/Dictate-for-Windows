#include "ffmpeg_bridge.h"

#include <errno.h>
#include <math.h>
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
#include <libavutil/mem.h>
#include <libavutil/opt.h>
#include <libavutil/samplefmt.h>
#include <libswresample/swresample.h>

static DictateLog dictate_log_sink = NULL;

static void dictate_log_callback(void *context, int level, const char *format, va_list args) {
    /* The Rust bridge serializes conversions because libav's log level is global.
     * Disabled FFmpeg debug still preserves native errors on stderr. */
    int limit = av_log_get_level();
    if (dictate_log_sink != NULL && limit >= AV_LOG_INFO && level <= limit) {
        char line[8192];
        int prefix = 1;
        va_list copy;
        va_copy(copy, args);
        av_log_format_line2(context, level, format, copy, line, sizeof(line), &prefix);
        va_end(copy);
        line[sizeof(line) - 1] = '\0';
        if (dictate_log_sink(line)) return;
    }
    av_log_default_callback(context, level, format, args);
}

void dictate_ffmpeg_set_log_callback(DictateLog callback) {
    dictate_log_sink = callback;
    av_log_set_callback(dictate_log_callback);
}

static void dictate_set_error(char *errbuf, int errbuf_size, const char *fmt, ...) {
    if (errbuf == NULL || errbuf_size <= 0) {
        return;
    }
    va_list args;
    va_start(args, fmt);
    vsnprintf(errbuf, errbuf_size, fmt, args);
    va_end(args);
}

static void dictate_set_av_error(char *errbuf, int errbuf_size, const char *prefix, int err) {
    char av_error[AV_ERROR_MAX_STRING_SIZE] = {0};
    av_strerror(err, av_error, sizeof(av_error));
    dictate_set_error(errbuf, errbuf_size, "%s: %s", prefix, av_error);
}

static int dictate_pick_sample_fmt(const AVCodec *codec, const char *requested) {
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

static int dictate_alloc_audio_frame(
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
        dictate_set_error(errbuf, errbuf_size, "could not allocate audio frame");
        return AVERROR(ENOMEM);
    }
    f->format = sample_fmt;
    f->sample_rate = sample_rate;
    f->nb_samples = nb_samples;
    ret = av_channel_layout_copy(&f->ch_layout, ch_layout);
    if (ret < 0) {
        av_frame_free(&f);
        dictate_set_av_error(errbuf, errbuf_size, "could not copy channel layout", ret);
        return ret;
    }
    if (nb_samples > 0) {
        ret = av_frame_get_buffer(f, 0);
        if (ret < 0) {
            av_frame_free(&f);
            dictate_set_av_error(errbuf, errbuf_size, "could not allocate audio frame buffer", ret);
            return ret;
        }
    }
    *frame = f;
    return 0;
}

static int dictate_encode_write(
    AVCodecContext *enc_ctx,
    AVFormatContext *ofmt_ctx,
    AVStream *out_stream,
    AVFrame *frame,
    char *errbuf,
    int errbuf_size
) {
    int ret = avcodec_send_frame(enc_ctx, frame);
    if (ret < 0) {
        dictate_set_av_error(errbuf, errbuf_size, "could not send frame to encoder", ret);
        return ret;
    }

    while (1) {
        AVPacket *pkt = av_packet_alloc();
        if (pkt == NULL) {
            dictate_set_error(errbuf, errbuf_size, "could not allocate encoded packet");
            return AVERROR(ENOMEM);
        }
        ret = avcodec_receive_packet(enc_ctx, pkt);
        if (ret == AVERROR(EAGAIN) || ret == AVERROR_EOF) {
            av_packet_free(&pkt);
            return 0;
        }
        if (ret < 0) {
            av_packet_free(&pkt);
            dictate_set_av_error(errbuf, errbuf_size, "could not receive encoded packet", ret);
            return ret;
        }
        av_packet_rescale_ts(pkt, enc_ctx->time_base, out_stream->time_base);
        pkt->stream_index = out_stream->index;
        ret = av_interleaved_write_frame(ofmt_ctx, pkt);
        av_packet_free(&pkt);
        if (ret < 0) {
            dictate_set_av_error(errbuf, errbuf_size, "could not write encoded packet", ret);
            return ret;
        }
    }
}

static int dictate_write_fifo_to_encoder(
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
        ret = dictate_alloc_audio_frame(
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
            dictate_set_error(errbuf, errbuf_size, "could not read converted samples from fifo");
            return AVERROR(EIO);
        }
        frame->pts = *next_pts;
        *next_pts += frame->nb_samples;
        ret = dictate_encode_write(enc_ctx, ofmt_ctx, out_stream, frame, errbuf, errbuf_size);
        av_frame_free(&frame);
        if (ret < 0) {
            return ret;
        }
    }
    return 0;
}

static int dictate_convert_and_queue_frame(
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
    int ret = dictate_alloc_audio_frame(
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
        dictate_set_av_error(errbuf, errbuf_size, "could not resample audio frame", ret);
        return ret;
    }
    converted->nb_samples = ret;
    if (ret > 0) {
        ret = av_audio_fifo_realloc(fifo, av_audio_fifo_size(fifo) + converted->nb_samples);
        if (ret < 0) {
            av_frame_free(&converted);
            dictate_set_av_error(errbuf, errbuf_size, "could not grow audio fifo", ret);
            return ret;
        }
        ret = av_audio_fifo_write(fifo, (void **)converted->extended_data, converted->nb_samples);
        if (ret < converted->nb_samples) {
            av_frame_free(&converted);
            dictate_set_error(errbuf, errbuf_size, "could not write converted samples to fifo");
            return AVERROR(EIO);
        }
    }
    av_frame_free(&converted);
    return 0;
}

static int dictate_flush_resampler(
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
        int ret = dictate_alloc_audio_frame(
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
            dictate_set_av_error(errbuf, errbuf_size, "could not flush resampler", ret);
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
            dictate_set_av_error(errbuf, errbuf_size, "could not grow audio fifo", ret);
            return ret;
        }
        ret = av_audio_fifo_write(fifo, (void **)converted->extended_data, converted->nb_samples);
        if (ret < converted->nb_samples) {
            av_frame_free(&converted);
            dictate_set_error(errbuf, errbuf_size, "could not write flushed samples to fifo");
            return AVERROR(EIO);
        }
        av_frame_free(&converted);
    }
}


typedef struct DictateGate {
    const DictateAudioInterval *intervals;
    size_t count, index;
    int enabled;
    int64_t position;
    DictateCancel cancel;
    void *context;
} DictateGate;

static int dictate_interrupt(void *opaque) {
    DictateGate *gate = opaque;
    return gate->cancel && gate->cancel(gate->context);
}

static int dictate_gate_frame(DictateGate *gate, SwrContext *swr, const AVFrame *input_format,
    AVCodecContext *enc, AVAudioFifo *fifo, AVFrame *frame, char *errbuf, int errbuf_size) {
    if (dictate_interrupt(gate)) return AVERROR_EXIT;
    if (frame->sample_rate != input_format->sample_rate ||
        frame->format != input_format->format ||
        av_channel_layout_compare(&frame->ch_layout, &input_format->ch_layout)) {
        dictate_set_error(errbuf, errbuf_size, "input audio format changed during decoding");
        return AVERROR_INVALIDDATA;
    }
    if (frame->nb_samples > INT64_MAX - gate->position) return AVERROR(EOVERFLOW);
    int64_t start = gate->position, end = start + frame->nb_samples;
    gate->position = end;
    if (!gate->enabled)
        return dictate_convert_and_queue_frame(swr, input_format, enc, fifo, frame, errbuf, errbuf_size);
    while (gate->index < gate->count) {
        if (dictate_interrupt(gate)) return AVERROR_EXIT;
        const DictateAudioInterval *i = &gate->intervals[gate->index];
        if (i->end_frame <= start) { gate->index++; continue; }
        if (i->start_frame >= end) break;
        int64_t left = FFMAX(start, i->start_frame), right = FFMIN(end, i->end_frame);
        AVFrame *selected = NULL;
        int ret = dictate_alloc_audio_frame(&selected, frame->format, &frame->ch_layout,
            frame->sample_rate, (int)(right-left), errbuf, errbuf_size);
        if (ret < 0) return ret;
        ret = av_samples_copy(selected->extended_data, frame->extended_data, 0,
            (int)(left-start), (int)(right-left), frame->ch_layout.nb_channels, frame->format);
        if (ret >= 0)
            ret = dictate_convert_and_queue_frame(swr, input_format, enc, fifo, selected, errbuf, errbuf_size);
        av_frame_free(&selected);
        if (ret < 0) return ret;
        if (i->end_frame <= end) gate->index++;
        else break;
    }
    return 0;
}

static int dictate_deliver_samples(AVAudioFifo *fifo, DictateSamples callback, void *context) {
    int16_t buffer[4096];
    while (av_audio_fifo_size(fifo) > 0) {
        int n = FFMIN(av_audio_fifo_size(fifo), 4096);
        void *planes[] = {buffer};
        if (av_audio_fifo_read(fifo, planes, n) != n) return AVERROR(EIO);
        if (callback(context, buffer, n)) return AVERROR_EXTERNAL;
    }
    return 0;
}

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
    DictateGate gate = {intervals, interval_count, 0, intervals_enabled, 0, cancel, cancel_context};
    if (intervals_enabled && (!interval_count || !intervals)) return AVERROR(EINVAL);
    for (size_t n = 0; n < interval_count; n++) {
        if (intervals[n].start_frame < 0 || intervals[n].end_frame <= intervals[n].start_frame ||
            (n && intervals[n].start_frame < intervals[n-1].end_frame)) return AVERROR(EINVAL);
    }
    ifmt_ctx = avformat_alloc_context();
    if (!ifmt_ctx) return AVERROR(ENOMEM);
    ifmt_ctx->interrupt_callback = (AVIOInterruptCB){dictate_interrupt, &gate};

    av_log_set_level(debug ? AV_LOG_INFO : AV_LOG_ERROR);

    ret = avformat_open_input(&ifmt_ctx, in_path, NULL, NULL);
    if (ret < 0) {
        dictate_set_av_error(errbuf, errbuf_size, "could not open input audio", ret);
        goto cleanup;
    }
    ret = avformat_find_stream_info(ifmt_ctx, NULL);
    if (ret < 0) {
        dictate_set_av_error(errbuf, errbuf_size, "could not read input stream info", ret);
        goto cleanup;
    }
    audio_stream = av_find_best_stream(ifmt_ctx, AVMEDIA_TYPE_AUDIO, -1, -1, NULL, 0);
    if (audio_stream < 0) {
        ret = audio_stream;
        dictate_set_av_error(errbuf, errbuf_size, "could not find input audio stream", ret);
        goto cleanup;
    }

    AVStream *in_stream = ifmt_ctx->streams[audio_stream];
    const AVCodec *decoder = avcodec_find_decoder(in_stream->codecpar->codec_id);
    if (decoder == NULL) {
        ret = AVERROR_DECODER_NOT_FOUND;
        dictate_set_error(errbuf, errbuf_size, "could not find decoder for input audio");
        goto cleanup;
    }
    dec_ctx = avcodec_alloc_context3(decoder);
    if (dec_ctx == NULL) {
        ret = AVERROR(ENOMEM);
        dictate_set_error(errbuf, errbuf_size, "could not allocate decoder context");
        goto cleanup;
    }
    ret = avcodec_parameters_to_context(dec_ctx, in_stream->codecpar);
    if (ret < 0) {
        dictate_set_av_error(errbuf, errbuf_size, "could not copy decoder parameters", ret);
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
        dictate_set_av_error(errbuf, errbuf_size, "could not open decoder", ret);
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
            dictate_set_av_error(errbuf, errbuf_size, "could not create output container", ret);
            goto cleanup;
        }
        const AVCodec *encoder = avcodec_find_encoder_by_name(codec_name);
        if (encoder == NULL) {
            ret = AVERROR_ENCODER_NOT_FOUND;
            dictate_set_error(errbuf, errbuf_size, "could not find encoder '%s'", codec_name);
            goto cleanup;
        }
        enc_ctx = avcodec_alloc_context3(encoder);
        if (enc_ctx == NULL) {
            ret = AVERROR(ENOMEM);
            dictate_set_error(errbuf, errbuf_size, "could not allocate encoder context");
            goto cleanup;
        }
        enc_ctx->sample_rate = sample_rate;
        enc_ctx->sample_fmt = dictate_pick_sample_fmt(encoder, sample_fmt_name);
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
            dictate_set_av_error(errbuf, errbuf_size, "could not open encoder", ret);
            goto cleanup;
        }

        out_stream = avformat_new_stream(ofmt_ctx, NULL);
        if (out_stream == NULL) {
            ret = AVERROR(ENOMEM);
            dictate_set_error(errbuf, errbuf_size, "could not allocate output stream");
            goto cleanup;
        }
        out_stream->time_base = enc_ctx->time_base;
        ret = avcodec_parameters_from_context(out_stream->codecpar, enc_ctx);
        if (ret < 0) {
            dictate_set_av_error(errbuf, errbuf_size, "could not copy encoder parameters", ret);
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
        dictate_set_av_error(errbuf, errbuf_size, "could not copy input channel layout", ret);
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
        dictate_set_av_error(errbuf, errbuf_size, "could not allocate resampler", ret);
        goto cleanup;
    }
    ret = swr_init(swr);
    if (ret < 0) {
        dictate_set_av_error(errbuf, errbuf_size, "could not initialize resampler", ret);
        goto cleanup;
    }

    fifo = av_audio_fifo_alloc(
        enc_ctx->sample_fmt,
        enc_ctx->ch_layout.nb_channels,
        enc_ctx->frame_size > 0 ? enc_ctx->frame_size : 1024
    );
    if (fifo == NULL) {
        ret = AVERROR(ENOMEM);
        dictate_set_error(errbuf, errbuf_size, "could not allocate audio fifo");
        goto cleanup;
    }

    if (!samples) {
        ofmt_ctx->interrupt_callback = (AVIOInterruptCB){dictate_interrupt, &gate};
        if (!(ofmt_ctx->oformat->flags & AVFMT_NOFILE)) {
            ret = avio_open2(&ofmt_ctx->pb, out_path, AVIO_FLAG_WRITE, &ofmt_ctx->interrupt_callback, NULL);
            output_opened = ret >= 0;
            if (ret < 0) {
                dictate_set_av_error(errbuf, errbuf_size, "could not open output audio", ret);
                goto cleanup;
            }
        }
        ret = avformat_write_header(ofmt_ctx, NULL);
        if (ret < 0) {
            dictate_set_av_error(errbuf, errbuf_size, "could not write output header", ret);
            goto cleanup;
        }

    }
    packet = av_packet_alloc();
    decoded = av_frame_alloc();
    if (packet == NULL || decoded == NULL) {
        ret = AVERROR(ENOMEM);
        dictate_set_error(errbuf, errbuf_size, "could not allocate decode buffers");
        goto cleanup;
    }

    while ((ret = av_read_frame(ifmt_ctx, packet)) >= 0) {
        if (dictate_interrupt(&gate)) { ret = AVERROR_EXIT; goto cleanup; }
        if (packet->stream_index != audio_stream) {
            av_packet_unref(packet);
            continue;
        }
        ret = avcodec_send_packet(dec_ctx, packet);
        av_packet_unref(packet);
        if (ret < 0) {
            dictate_set_av_error(errbuf, errbuf_size, "could not send packet to decoder", ret);
            goto cleanup;
        }
        while ((ret = avcodec_receive_frame(dec_ctx, decoded)) >= 0) {
            ret = dictate_gate_frame(&gate,
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
            ret = samples ? dictate_deliver_samples(fifo, samples, samples_context) : dictate_write_fifo_to_encoder(
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
            dictate_set_av_error(errbuf, errbuf_size, "could not receive decoded frame", ret);
            goto cleanup;
        }
    }
    if (ret != AVERROR_EOF) {
        dictate_set_av_error(errbuf, errbuf_size, "could not read input packet", ret);
        goto cleanup;
    }

    ret = avcodec_send_packet(dec_ctx, NULL);
    if (ret < 0) {
        dictate_set_av_error(errbuf, errbuf_size, "could not flush decoder", ret);
        goto cleanup;
    }
    while ((ret = avcodec_receive_frame(dec_ctx, decoded)) >= 0) {
        ret = dictate_gate_frame(&gate,
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
        ret = samples ? dictate_deliver_samples(fifo, samples, samples_context) : dictate_write_fifo_to_encoder(
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
        dictate_set_av_error(errbuf, errbuf_size, "could not receive flushed decoded frame", ret);
        goto cleanup;
    }

    ret = dictate_flush_resampler(swr, input_format, enc_ctx, fifo, errbuf, errbuf_size);
    if (ret < 0) {
        goto cleanup;
    }
    ret = samples ? dictate_deliver_samples(fifo, samples, samples_context) : dictate_write_fifo_to_encoder(
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
        ret = dictate_encode_write(enc_ctx, ofmt_ctx, out_stream, NULL, errbuf, errbuf_size);
        if (ret < 0) {
            goto cleanup;
        }
        ret = av_write_trailer(ofmt_ctx);
        if (ret < 0) {
            dictate_set_av_error(errbuf, errbuf_size, "could not write output trailer", ret);
            goto cleanup;
        }
        ret = 0;

    }
    if (intervals_enabled && gate.position < intervals[interval_count-1].end_frame) {
        ret = AVERROR_INVALIDDATA;
        dictate_set_error(errbuf, errbuf_size, "input ended before selected intervals");
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
    if (dictate_interrupt(&gate)) ret = AVERROR_EXIT;
    if (ret < 0 && output_opened) remove(out_path);
    return ret;
}

/* Segmented upload uses the decoder's source-frame domain throughout.  The
 * shared scan below establishes the exact coordinates used by both silence
 * analysis and export; silence analysis subsequently downmixes with
 * swresample at the same source rate, but never invokes the speech VAD.
 * That keeps every chosen pause boundary aligned with the exporter. */
typedef int (*DictateDecodedFrameCallback)(
    const AVFrame *frame,
    const AVFrame *input_format,
    int64_t start_frame,
    int64_t end_frame,
    void *opaque,
    char *errbuf,
    int errbuf_size
);

static int dictate_validate_source_frame(
    const AVFrame *input_format,
    const AVFrame *frame,
    char *errbuf,
    int errbuf_size
) {
    if (frame->sample_rate != input_format->sample_rate ||
        frame->format != input_format->format ||
        av_channel_layout_compare(&frame->ch_layout, &input_format->ch_layout)) {
        dictate_set_error(errbuf, errbuf_size, "input audio format changed during decoding");
        return AVERROR_INVALIDDATA;
    }
    return 0;
}

/* Decode the input once and expose every decoded source frame to a caller.
 * `start_frame`/`end_frame` are per-channel sample-frame coordinates, not
 * bytes or encoded packet timestamps. */
static int dictate_scan_decoded_input(
    const char *in_path,
    int debug,
    DictateCancel cancel,
    void *cancel_context,
    DictateDecodedFrameCallback callback,
    void *callback_context,
    int *source_rate,
    int64_t *source_frames,
    char *errbuf,
    int errbuf_size
) {
    AVFormatContext *ifmt_ctx = NULL;
    AVCodecContext *dec_ctx = NULL;
    AVPacket *packet = NULL;
    AVFrame *decoded = NULL;
    AVFrame *input_format = NULL;
    int audio_stream = -1;
    int ret = 0;
    DictateGate gate = {NULL, 0, 0, 0, 0, cancel, cancel_context};

    if (!in_path || !callback) return AVERROR(EINVAL);
    ifmt_ctx = avformat_alloc_context();
    if (!ifmt_ctx) return AVERROR(ENOMEM);
    ifmt_ctx->interrupt_callback = (AVIOInterruptCB){dictate_interrupt, &gate};
    av_log_set_level(debug ? AV_LOG_INFO : AV_LOG_ERROR);

    ret = avformat_open_input(&ifmt_ctx, in_path, NULL, NULL);
    if (ret < 0) {
        dictate_set_av_error(errbuf, errbuf_size, "could not open input audio", ret);
        goto cleanup;
    }
    ret = avformat_find_stream_info(ifmt_ctx, NULL);
    if (ret < 0) {
        dictate_set_av_error(errbuf, errbuf_size, "could not read input stream info", ret);
        goto cleanup;
    }
    audio_stream = av_find_best_stream(ifmt_ctx, AVMEDIA_TYPE_AUDIO, -1, -1, NULL, 0);
    if (audio_stream < 0) {
        ret = audio_stream;
        dictate_set_av_error(errbuf, errbuf_size, "could not find input audio stream", ret);
        goto cleanup;
    }

    AVStream *in_stream = ifmt_ctx->streams[audio_stream];
    const AVCodec *decoder = avcodec_find_decoder(in_stream->codecpar->codec_id);
    if (!decoder) {
        ret = AVERROR_DECODER_NOT_FOUND;
        dictate_set_error(errbuf, errbuf_size, "could not find decoder for input audio");
        goto cleanup;
    }
    dec_ctx = avcodec_alloc_context3(decoder);
    if (!dec_ctx) {
        ret = AVERROR(ENOMEM);
        dictate_set_error(errbuf, errbuf_size, "could not allocate decoder context");
        goto cleanup;
    }
    ret = avcodec_parameters_to_context(dec_ctx, in_stream->codecpar);
    if (ret < 0) {
        dictate_set_av_error(errbuf, errbuf_size, "could not copy decoder parameters", ret);
        goto cleanup;
    }
    if (dec_ctx->ch_layout.nb_channels <= 0) {
        int input_channels = in_stream->codecpar->ch_layout.nb_channels;
        av_channel_layout_default(&dec_ctx->ch_layout, input_channels > 0 ? input_channels : 1);
    }
    ret = avcodec_open2(dec_ctx, decoder, NULL);
    if (ret < 0) {
        dictate_set_av_error(errbuf, errbuf_size, "could not open decoder", ret);
        goto cleanup;
    }

    input_format = av_frame_alloc();
    if (!input_format) {
        ret = AVERROR(ENOMEM);
        goto cleanup;
    }
    input_format->sample_rate = dec_ctx->sample_rate;
    input_format->format = dec_ctx->sample_fmt;
    ret = av_channel_layout_copy(&input_format->ch_layout, &dec_ctx->ch_layout);
    if (ret < 0) {
        dictate_set_av_error(errbuf, errbuf_size, "could not copy input channel layout", ret);
        goto cleanup;
    }
    if (input_format->sample_rate <= 0 || input_format->ch_layout.nb_channels <= 0) {
        ret = AVERROR_INVALIDDATA;
        dictate_set_error(errbuf, errbuf_size, "input audio has no valid sample format");
        goto cleanup;
    }

    packet = av_packet_alloc();
    decoded = av_frame_alloc();
    if (!packet || !decoded) {
        ret = AVERROR(ENOMEM);
        dictate_set_error(errbuf, errbuf_size, "could not allocate decode buffers");
        goto cleanup;
    }

    while ((ret = av_read_frame(ifmt_ctx, packet)) >= 0) {
        if (dictate_interrupt(&gate)) {
            ret = AVERROR_EXIT;
            goto cleanup;
        }
        if (packet->stream_index != audio_stream) {
            av_packet_unref(packet);
            continue;
        }
        ret = avcodec_send_packet(dec_ctx, packet);
        av_packet_unref(packet);
        if (ret < 0) {
            dictate_set_av_error(errbuf, errbuf_size, "could not send packet to decoder", ret);
            goto cleanup;
        }
        while ((ret = avcodec_receive_frame(dec_ctx, decoded)) >= 0) {
            ret = dictate_validate_source_frame(input_format, decoded, errbuf, errbuf_size);
            if (ret >= 0) {
                if (decoded->nb_samples < 0 || decoded->nb_samples > INT64_MAX - gate.position) {
                    ret = AVERROR(EOVERFLOW);
                    dictate_set_error(errbuf, errbuf_size, "source audio frame count overflow");
                } else {
                    int64_t start = gate.position;
                    int64_t end = start + decoded->nb_samples;
                    ret = callback(decoded, input_format, start, end, callback_context, errbuf, errbuf_size);
                    if (ret >= 0) gate.position = end;
                }
            }
            av_frame_unref(decoded);
            if (ret < 0) goto cleanup;
        }
        if (ret != AVERROR(EAGAIN) && ret != AVERROR_EOF) {
            dictate_set_av_error(errbuf, errbuf_size, "could not receive decoded frame", ret);
            goto cleanup;
        }
    }
    if (ret != AVERROR_EOF) {
        dictate_set_av_error(errbuf, errbuf_size, "could not read input packet", ret);
        goto cleanup;
    }

    ret = avcodec_send_packet(dec_ctx, NULL);
    if (ret < 0) {
        dictate_set_av_error(errbuf, errbuf_size, "could not flush decoder", ret);
        goto cleanup;
    }
    while ((ret = avcodec_receive_frame(dec_ctx, decoded)) >= 0) {
        ret = dictate_validate_source_frame(input_format, decoded, errbuf, errbuf_size);
        if (ret >= 0) {
            if (decoded->nb_samples < 0 || decoded->nb_samples > INT64_MAX - gate.position) {
                ret = AVERROR(EOVERFLOW);
                dictate_set_error(errbuf, errbuf_size, "source audio frame count overflow");
            } else {
                int64_t start = gate.position;
                int64_t end = start + decoded->nb_samples;
                ret = callback(decoded, input_format, start, end, callback_context, errbuf, errbuf_size);
                if (ret >= 0) gate.position = end;
            }
        }
        av_frame_unref(decoded);
        if (ret < 0) goto cleanup;
    }
    if (ret != AVERROR_EOF && ret != AVERROR(EAGAIN)) {
        dictate_set_av_error(errbuf, errbuf_size, "could not receive flushed decoded frame", ret);
        goto cleanup;
    }
    ret = 0;
    if (source_rate) *source_rate = input_format->sample_rate;
    if (source_frames) *source_frames = gate.position;

cleanup:
    av_frame_free(&input_format);
    av_frame_free(&decoded);
    av_packet_free(&packet);
    avcodec_free_context(&dec_ctx);
    if (ifmt_ctx) avformat_close_input(&ifmt_ctx);
    if (dictate_interrupt(&gate)) ret = AVERROR_EXIT;
    return ret;
}

/* Split-mode silence detection in ASR-Audio-Preprocess runs an aformat filter
 * to mono/double before inspecting individual samples.  This bridge has no
 * libavfilter, so use swresample for the equivalent conversion while retaining
 * the source rate.  Produced mono samples are numbered continuously and must
 * exactly equal the decoded source-frame count after the resampler is flushed. */
typedef int (*DictateMonoSamplesCallback)(
    const double *samples,
    int count,
    int64_t start_frame,
    void *opaque,
    char *errbuf,
    int errbuf_size
);

typedef struct DictateMonoCapture {
    SwrContext *swr;
    int source_rate;
    int64_t produced_frames;
    DictateMonoSamplesCallback callback;
    void *callback_context;
    DictateCancel cancel;
    void *cancel_context;
} DictateMonoCapture;

static int dictate_deliver_mono_samples(
    DictateMonoCapture *capture,
    const double *samples,
    int count,
    char *errbuf,
    int errbuf_size
) {
    if (count <= 0) return 0;
    if (count > INT64_MAX - capture->produced_frames) {
        dictate_set_error(errbuf, errbuf_size, "mono source frame count overflow");
        return AVERROR(EOVERFLOW);
    }
    int ret = capture->callback(
        samples,
        count,
        capture->produced_frames,
        capture->callback_context,
        errbuf,
        errbuf_size
    );
    if (ret >= 0) capture->produced_frames += count;
    return ret;
}

static int dictate_init_mono_capture(
    DictateMonoCapture *capture,
    const AVFrame *input_format,
    char *errbuf,
    int errbuf_size
) {
    if (capture->swr) return 0;
    if (input_format->sample_rate <= 0 || input_format->ch_layout.nb_channels <= 0) {
        dictate_set_error(errbuf, errbuf_size, "input audio has no valid sample format");
        return AVERROR_INVALIDDATA;
    }
    AVChannelLayout mono = {0};
    av_channel_layout_default(&mono, 1);
    int ret = swr_alloc_set_opts2(
        &capture->swr,
        &mono,
        AV_SAMPLE_FMT_DBL,
        input_format->sample_rate,
        &input_format->ch_layout,
        input_format->format,
        input_format->sample_rate,
        0,
        NULL
    );
    av_channel_layout_uninit(&mono);
    if (ret < 0) {
        dictate_set_av_error(errbuf, errbuf_size, "could not allocate mono silence resampler", ret);
        return ret;
    }
    ret = swr_init(capture->swr);
    if (ret < 0) {
        dictate_set_av_error(errbuf, errbuf_size, "could not initialize mono silence resampler", ret);
        return ret;
    }
    capture->source_rate = input_format->sample_rate;
    return 0;
}

static int dictate_mono_frame(
    const AVFrame *frame,
    const AVFrame *input_format,
    int64_t start_frame,
    int64_t end_frame,
    void *opaque,
    char *errbuf,
    int errbuf_size
) {
    (void)start_frame;
    (void)end_frame;
    DictateMonoCapture *capture = opaque;
    if (capture->cancel && capture->cancel(capture->cancel_context)) return AVERROR_EXIT;
    int ret = dictate_init_mono_capture(capture, input_format, errbuf, errbuf_size);
    if (ret < 0) return ret;
    int64_t delay = swr_get_delay(capture->swr, capture->source_rate);
    if (delay < 0 || delay > INT_MAX - frame->nb_samples) {
        dictate_set_error(errbuf, errbuf_size, "mono silence resampler delay overflow");
        return AVERROR(EOVERFLOW);
    }
    int capacity = (int)delay + frame->nb_samples;
    if (capacity <= 0) return 0;
    double *samples = av_malloc_array(capacity, sizeof(*samples));
    if (!samples) {
        dictate_set_error(errbuf, errbuf_size, "could not allocate mono silence samples");
        return AVERROR(ENOMEM);
    }
    uint8_t *planes[] = {(uint8_t *)samples};
    ret = swr_convert(
        capture->swr,
        planes,
        capacity,
        (const uint8_t **)frame->extended_data,
        frame->nb_samples
    );
    if (ret < 0) {
        av_free(samples);
        dictate_set_av_error(errbuf, errbuf_size, "could not resample mono silence samples", ret);
        return ret;
    }
    int deliver_ret = dictate_deliver_mono_samples(capture, samples, ret, errbuf, errbuf_size);
    av_free(samples);
    return deliver_ret;
}

static int dictate_flush_mono_capture(
    DictateMonoCapture *capture,
    int64_t expected_frames,
    char *errbuf,
    int errbuf_size
) {
    if (!capture->swr) {
        if (expected_frames == 0) return 0;
        dictate_set_error(errbuf, errbuf_size, "mono silence resampler did not receive source frames");
        return AVERROR_INVALIDDATA;
    }
    while (1) {
        if (capture->cancel && capture->cancel(capture->cancel_context)) return AVERROR_EXIT;
        int64_t delay = swr_get_delay(capture->swr, capture->source_rate);
        if (delay <= 0) break;
        if (delay > INT_MAX) {
            dictate_set_error(errbuf, errbuf_size, "mono silence resampler delay overflow");
            return AVERROR(EOVERFLOW);
        }
        int capacity = (int)delay;
        if (capacity < 1) capacity = 1;
        double *samples = av_malloc_array(capacity, sizeof(*samples));
        if (!samples) {
            dictate_set_error(errbuf, errbuf_size, "could not allocate flushed mono silence samples");
            return AVERROR(ENOMEM);
        }
        uint8_t *planes[] = {(uint8_t *)samples};
        int ret = swr_convert(capture->swr, planes, capacity, NULL, 0);
        if (ret < 0) {
            av_free(samples);
            dictate_set_av_error(errbuf, errbuf_size, "could not flush mono silence resampler", ret);
            return ret;
        }
        int deliver_ret = dictate_deliver_mono_samples(capture, samples, ret, errbuf, errbuf_size);
        av_free(samples);
        if (deliver_ret < 0) return deliver_ret;
        if (ret == 0) break;
    }
    if (capture->produced_frames != expected_frames) {
        dictate_set_error(errbuf, errbuf_size, "mono silence samples do not match source-frame coordinates");
        return AVERROR_INVALIDDATA;
    }
    return 0;
}

static int dictate_scan_mono_f64(
    const char *in_path,
    int debug,
    DictateCancel cancel,
    void *cancel_context,
    DictateMonoSamplesCallback callback,
    void *callback_context,
    int *source_rate,
    int64_t *source_frames,
    char *errbuf,
    int errbuf_size
) {
    DictateMonoCapture capture = {
        .callback = callback,
        .callback_context = callback_context,
        .cancel = cancel,
        .cancel_context = cancel_context,
    };
    int rate = 0;
    int64_t frames = 0;
    int ret = dictate_scan_decoded_input(
        in_path,
        debug,
        cancel,
        cancel_context,
        dictate_mono_frame,
        &capture,
        &rate,
        &frames,
        errbuf,
        errbuf_size
    );
    if (ret >= 0) ret = dictate_flush_mono_capture(&capture, frames, errbuf, errbuf_size);
    if (ret >= 0 && capture.source_rate != rate && frames > 0) {
        dictate_set_error(errbuf, errbuf_size, "mono silence source rate differs from decoded source rate");
        ret = AVERROR_INVALIDDATA;
    }
    if (source_rate) *source_rate = rate;
    if (source_frames) *source_frames = frames;
    swr_free(&capture.swr);
    return ret;
}

typedef struct DictateVolumeCapture {
    double sum_square;
    double max_abs;
    uint64_t sample_count;
    int has_max;
    int has_mean;
} DictateVolumeCapture;

static int dictate_collect_volume(
    const double *samples,
    int count,
    int64_t start_frame,
    void *opaque,
    char *errbuf,
    int errbuf_size
) {
    (void)start_frame;
    DictateVolumeCapture *capture = opaque;
    for (int sample = 0; sample < count; sample++) {
        double value = samples[sample];
        if (!isfinite(value)) continue;
        double abs_value = fabs(value);
        capture->sum_square += value * value;
        if (abs_value > capture->max_abs) capture->max_abs = abs_value;
        if (capture->sample_count == UINT64_MAX) {
            dictate_set_error(errbuf, errbuf_size, "source audio frame count overflow");
            return AVERROR(EOVERFLOW);
        }
        capture->sample_count++;
        capture->has_max = 1;
        capture->has_mean = 1;
    }
    return 0;
}

typedef struct DictateSilenceCapture {
    DictateAudioInterval *items;
    size_t count;
    size_t capacity;
    double threshold;
    int64_t min_silence_frames;
    int pending;
    int64_t pending_start;
} DictateSilenceCapture;

static int dictate_push_silence(
    DictateSilenceCapture *capture,
    int64_t start_frame,
    int64_t end_frame
) {
    if (end_frame <= start_frame) return 0;
    if (capture->count == capture->capacity) {
        size_t next = capture->capacity ? capture->capacity * 2 : 8;
        if (next < capture->capacity || next > SIZE_MAX / sizeof(*capture->items)) {
            return AVERROR(ENOMEM);
        }
        DictateAudioInterval *items = av_realloc_array(capture->items, next, sizeof(*capture->items));
        if (!items) return AVERROR(ENOMEM);
        capture->items = items;
        capture->capacity = next;
    }
    capture->items[capture->count++] = (DictateAudioInterval){start_frame, end_frame};
    return 0;
}

static int dictate_collect_silence(
    const double *samples,
    int count,
    int64_t start_frame,
    void *opaque,
    char *errbuf,
    int errbuf_size
) {
    DictateSilenceCapture *capture = opaque;
    for (int sample = 0; sample < count; sample++) {
        double value = samples[sample];
        int64_t position = start_frame + sample;
        int silent = !isnan(value) && fabs(value) <= capture->threshold;
        if (silent) {
            if (!capture->pending) {
                capture->pending = 1;
                capture->pending_start = position;
            }
            continue;
        }
        if (capture->pending) {
            if (position - capture->pending_start >= capture->min_silence_frames) {
                int ret = dictate_push_silence(capture, capture->pending_start, position);
                if (ret < 0) {
                    dictate_set_error(errbuf, errbuf_size, "could not allocate silence intervals");
                    return ret;
                }
            }
            capture->pending = 0;
        }
    }
    return 0;
}

static int dictate_finish_silence_capture(
    DictateSilenceCapture *capture,
    int64_t total_frames,
    char *errbuf,
    int errbuf_size
) {
    if (!capture->pending) return 0;
    /*
     * A source that stayed silent from frame zero has no speech to preserve
     * or a pause boundary to choose.  Report it even when it is shorter than
     * the configured minimum pause: Rust then recognizes [0, total) as the
     * explicit no-speech outcome and sends no segment.  Ordinary trailing
     * pauses remain subject to the configured minimum.
     */
    if (capture->pending_start == 0 ||
        total_frames - capture->pending_start >= capture->min_silence_frames) {
        int ret = dictate_push_silence(capture, capture->pending_start, total_frames);
        if (ret < 0) {
            dictate_set_error(errbuf, errbuf_size, "could not allocate silence intervals");
            return ret;
        }
    }
    capture->pending = 0;
    return 0;
}

static size_t dictate_auto_silence_thresholds(
    const DictateVolumeCapture *capture,
    double *thresholds,
    size_t capacity
) {
    static const double max_offsets[] = {18.0, 16.0, 14.0, 12.0, 10.0};
    static const double fallback[] = {-35.0, -30.0, -25.0, -20.0};
    if (!capture || !thresholds || capacity == 0) return 0;
    size_t count = 0;
    if (capture->has_max && capture->max_abs > 0.0 && isfinite(capture->max_abs)) {
        double max_db = 20.0 * log10(capture->max_abs);
        for (size_t n = 0; n < sizeof(max_offsets) / sizeof(max_offsets[0]) && count < capacity; n++) {
            double db = max_db - max_offsets[n];
            if (db < -60.0) db = -60.0;
            if (db > -10.0) db = -10.0;
            if (count == 0 || thresholds[count - 1] != db) thresholds[count++] = db;
        }
        return count;
    }
    if (capture->has_mean && capture->sample_count > 0 && capture->sum_square > 0.0 &&
        isfinite(capture->sum_square)) {
        double mean_db = 10.0 * log10(capture->sum_square / (double)capture->sample_count);
        double base = mean_db - 8.0;
        static const double mean_offsets[] = {0.0, 6.0, 12.0};
        for (size_t n = 0; n < sizeof(mean_offsets) / sizeof(mean_offsets[0]) && count < capacity; n++) {
            double db = base + mean_offsets[n];
            if (db < -60.0) db = -60.0;
            if (db > -10.0) db = -10.0;
            if (count == 0 || thresholds[count - 1] != db) thresholds[count++] = db;
        }
        return count;
    }
    for (size_t n = 0; n < sizeof(fallback) / sizeof(fallback[0]) && count < capacity; n++) {
        thresholds[count++] = fallback[n];
    }
    return count;
}

void dictate_ffmpeg_free_intervals(DictateAudioInterval *intervals) {
    av_free(intervals);
}

int dictate_ffmpeg_analyze_silence(
    const char *in_path,
    int64_t min_pause_ms,
    int debug,
    DictateCancel cancel,
    void *cancel_context,
    DictateAudioInterval **intervals,
    size_t *interval_count,
    int *source_rate,
    int64_t *source_frames,
    char *errbuf,
    int errbuf_size
) {
    DictateVolumeCapture volume = {0};
    int rate = 0;
    int64_t frames = 0;
    int ret = 0;
    if (!in_path || !intervals || !interval_count) return AVERROR(EINVAL);
    *intervals = NULL;
    *interval_count = 0;
    if (source_rate) *source_rate = 0;
    if (source_frames) *source_frames = 0;
    if (min_pause_ms < 1) min_pause_ms = 1;

    ret = dictate_scan_mono_f64(
        in_path, debug, cancel, cancel_context, dictate_collect_volume, &volume,
        &rate, &frames, errbuf, errbuf_size
    );
    if (ret < 0) return ret;
    if (rate <= 0 || frames < 0) {
        dictate_set_error(errbuf, errbuf_size, "input audio has no valid source frame domain");
        return AVERROR_INVALIDDATA;
    }
    if (source_rate) *source_rate = rate;
    if (source_frames) *source_frames = frames;
    if (frames == 0) return 0;

    int64_t min_frames = av_rescale_rnd(min_pause_ms, rate, 1000, AV_ROUND_UP);
    if (min_frames <= 0) min_frames = 1;
    double thresholds[5] = {0};
    size_t threshold_count = dictate_auto_silence_thresholds(&volume, thresholds, 5);
    for (size_t n = 0; n < threshold_count; n++) {
        DictateSilenceCapture capture = {
            .threshold = pow(10.0, thresholds[n] / 20.0),
            .min_silence_frames = min_frames,
        };
        int current_rate = 0;
        int64_t current_frames = 0;
        ret = dictate_scan_mono_f64(
            in_path, debug, cancel, cancel_context, dictate_collect_silence, &capture,
            &current_rate, &current_frames, errbuf, errbuf_size
        );
        if (ret >= 0) {
            ret = dictate_finish_silence_capture(&capture, current_frames, errbuf, errbuf_size);
        }
        if (ret < 0) {
            av_free(capture.items);
            return ret;
        }
        if (current_rate != rate || current_frames != frames) {
            av_free(capture.items);
            dictate_set_error(errbuf, errbuf_size, "input audio changed while analyzing silence");
            return AVERROR_INVALIDDATA;
        }
        if (capture.count > 0) {
            *intervals = capture.items;
            *interval_count = capture.count;
            return 0;
        }
        av_free(capture.items);
    }
    return 0;
}

typedef struct DictateSegmentOutput {
    AVFormatContext *ofmt_ctx;
    AVCodecContext *enc_ctx;
    AVAudioFifo *fifo;
    SwrContext *swr;
    AVStream *out_stream;
    int64_t next_pts;
} DictateSegmentOutput;

static const char *dictate_output_format_for_path(const char *out_path) {
    const char *extension = out_path ? strrchr(out_path, '.') : NULL;
    static const char *const raw_formats[] = {
        "s8", "s16le", "s16be", "s24le", "s24be", "s32le", "s32be",
        "f32le", "f32be", "f64le", "f64be", "alaw", "mulaw"
    };
    if (!extension || !extension[1]) return NULL;
    if (av_strcasecmp(extension + 1, "mka") == 0) return "matroska";
    for (size_t n = 0; n < sizeof(raw_formats) / sizeof(raw_formats[0]); n++) {
        if (av_strcasecmp(extension + 1, raw_formats[n]) == 0) return raw_formats[n];
    }
    return NULL;
}

static int dictate_dispose_segment_output(DictateSegmentOutput *output) {
    int ret = 0;
    if (!output) return 0;
    av_audio_fifo_free(output->fifo);
    output->fifo = NULL;
    swr_free(&output->swr);
    avcodec_free_context(&output->enc_ctx);
    if (output->ofmt_ctx) {
        if (!(output->ofmt_ctx->oformat->flags & AVFMT_NOFILE) && output->ofmt_ctx->pb) {
            ret = avio_closep(&output->ofmt_ctx->pb);
        }
        avformat_free_context(output->ofmt_ctx);
    }
    memset(output, 0, sizeof(*output));
    return ret;
}

static int dictate_open_segment_output(
    DictateSegmentOutput *output,
    const char *out_path,
    const char *codec_name,
    int channels,
    int sample_rate,
    int bitrate_kbps,
    int codec_has_bitrate,
    const char *sample_fmt_name,
    const AVFrame *input_format,
    DictateGate *gate,
    char *errbuf,
    int errbuf_size
) {
    int ret = 0;
    const char *output_format = NULL;
    if (!output || !out_path || !*out_path || !codec_name || !*codec_name ||
        channels <= 0 || sample_rate <= 0) {
        return AVERROR(EINVAL);
    }
    memset(output, 0, sizeof(*output));
    output_format = dictate_output_format_for_path(out_path);
    ret = avformat_alloc_output_context2(&output->ofmt_ctx, NULL, output_format, out_path);
    if (ret < 0 || !output->ofmt_ctx) {
        if (ret >= 0) ret = AVERROR(ENOMEM);
        dictate_set_av_error(errbuf, errbuf_size, "could not create segment output container", ret);
        return ret;
    }
    const AVCodec *encoder = avcodec_find_encoder_by_name(codec_name);
    if (!encoder) {
        ret = AVERROR_ENCODER_NOT_FOUND;
        dictate_set_error(errbuf, errbuf_size, "could not find encoder '%s'", codec_name);
        return ret;
    }
    output->enc_ctx = avcodec_alloc_context3(encoder);
    if (!output->enc_ctx) {
        ret = AVERROR(ENOMEM);
        dictate_set_error(errbuf, errbuf_size, "could not allocate segment encoder context");
        return ret;
    }
    output->enc_ctx->sample_rate = sample_rate;
    output->enc_ctx->sample_fmt = dictate_pick_sample_fmt(encoder, sample_fmt_name);
    output->enc_ctx->time_base = (AVRational){1, sample_rate};
    if (codec_has_bitrate) output->enc_ctx->bit_rate = (int64_t)bitrate_kbps * 1000;
    av_channel_layout_default(&output->enc_ctx->ch_layout, channels);
    if (output->ofmt_ctx->oformat->flags & AVFMT_GLOBALHEADER) {
        output->enc_ctx->flags |= AV_CODEC_FLAG_GLOBAL_HEADER;
    }
    ret = avcodec_open2(output->enc_ctx, encoder, NULL);
    if (ret < 0) {
        dictate_set_av_error(errbuf, errbuf_size, "could not open segment encoder", ret);
        return ret;
    }
    output->out_stream = avformat_new_stream(output->ofmt_ctx, NULL);
    if (!output->out_stream) {
        ret = AVERROR(ENOMEM);
        dictate_set_error(errbuf, errbuf_size, "could not allocate segment output stream");
        return ret;
    }
    output->out_stream->time_base = output->enc_ctx->time_base;
    ret = avcodec_parameters_from_context(output->out_stream->codecpar, output->enc_ctx);
    if (ret < 0) {
        dictate_set_av_error(errbuf, errbuf_size, "could not copy segment encoder parameters", ret);
        return ret;
    }
    ret = swr_alloc_set_opts2(
        &output->swr,
        &output->enc_ctx->ch_layout,
        output->enc_ctx->sample_fmt,
        output->enc_ctx->sample_rate,
        &input_format->ch_layout,
        input_format->format,
        input_format->sample_rate,
        0,
        NULL
    );
    if (ret < 0) {
        dictate_set_av_error(errbuf, errbuf_size, "could not allocate segment resampler", ret);
        return ret;
    }
    ret = swr_init(output->swr);
    if (ret < 0) {
        dictate_set_av_error(errbuf, errbuf_size, "could not initialize segment resampler", ret);
        return ret;
    }
    output->fifo = av_audio_fifo_alloc(
        output->enc_ctx->sample_fmt,
        output->enc_ctx->ch_layout.nb_channels,
        output->enc_ctx->frame_size > 0 ? output->enc_ctx->frame_size : 1024
    );
    if (!output->fifo) {
        ret = AVERROR(ENOMEM);
        dictate_set_error(errbuf, errbuf_size, "could not allocate segment audio fifo");
        return ret;
    }
    output->ofmt_ctx->interrupt_callback = (AVIOInterruptCB){dictate_interrupt, gate};
    if (!(output->ofmt_ctx->oformat->flags & AVFMT_NOFILE)) {
        ret = avio_open2(
            &output->ofmt_ctx->pb,
            out_path,
            AVIO_FLAG_WRITE,
            &output->ofmt_ctx->interrupt_callback,
            NULL
        );
        if (ret < 0) {
            dictate_set_av_error(errbuf, errbuf_size, "could not open segment output audio", ret);
            return ret;
        }
    }
    ret = avformat_write_header(output->ofmt_ctx, NULL);
    if (ret < 0) {
        dictate_set_av_error(errbuf, errbuf_size, "could not write segment output header", ret);
        return ret;
    }
    return 0;
}

static int dictate_finish_segment_output(
    DictateSegmentOutput *output,
    const AVFrame *input_format,
    DictateGate *gate,
    char *errbuf,
    int errbuf_size
) {
    int ret = 0;
    if (dictate_interrupt(gate)) ret = AVERROR_EXIT;
    if (ret >= 0) {
        ret = dictate_flush_resampler(
            output->swr,
            input_format,
            output->enc_ctx,
            output->fifo,
            errbuf,
            errbuf_size
        );
    }
    if (ret >= 0 && dictate_interrupt(gate)) ret = AVERROR_EXIT;
    if (ret >= 0) {
        ret = dictate_write_fifo_to_encoder(
            output->fifo,
            output->enc_ctx,
            output->ofmt_ctx,
            output->out_stream,
            1,
            &output->next_pts,
            errbuf,
            errbuf_size
        );
    }
    if (ret >= 0 && dictate_interrupt(gate)) ret = AVERROR_EXIT;
    if (ret >= 0) {
        ret = dictate_encode_write(
            output->enc_ctx,
            output->ofmt_ctx,
            output->out_stream,
            NULL,
            errbuf,
            errbuf_size
        );
    }
    if (ret >= 0 && dictate_interrupt(gate)) ret = AVERROR_EXIT;
    if (ret >= 0) {
        ret = av_write_trailer(output->ofmt_ctx);
        if (ret < 0) dictate_set_av_error(errbuf, errbuf_size, "could not write segment output trailer", ret);
    }
    int close_ret = dictate_dispose_segment_output(output);
    if (ret >= 0 && close_ret < 0) {
        dictate_set_av_error(errbuf, errbuf_size, "could not close segment output audio", close_ret);
        ret = close_ret;
    }
    return ret;
}

typedef struct DictateExportCapture {
    const char *const *out_paths;
    const DictateAudioInterval *intervals;
    size_t count;
    size_t index;
    int *touched;
    int expected_source_rate;
    const char *codec_name;
    int channels;
    int sample_rate;
    int bitrate_kbps;
    int codec_has_bitrate;
    const char *sample_fmt_name;
    DictateGate gate;
    DictateSegmentOutput active;
    int active_open;
} DictateExportCapture;

static int dictate_export_frame(
    const AVFrame *frame,
    const AVFrame *input_format,
    int64_t start_frame,
    int64_t end_frame,
    void *opaque,
    char *errbuf,
    int errbuf_size
) {
    DictateExportCapture *capture = opaque;
    if (input_format->sample_rate != capture->expected_source_rate) {
        dictate_set_error(errbuf, errbuf_size, "input source sample rate differs from segment plan");
        return AVERROR_INVALIDDATA;
    }
    while (capture->index < capture->count) {
        if (dictate_interrupt(&capture->gate)) return AVERROR_EXIT;
        const DictateAudioInterval *interval = &capture->intervals[capture->index];
        if (interval->end_frame <= start_frame) {
            dictate_set_error(errbuf, errbuf_size, "segment plan did not consume a source interval");
            return AVERROR_INVALIDDATA;
        }
        if (interval->start_frame >= end_frame) return 0;
        int64_t left = FFMAX(start_frame, interval->start_frame);
        int64_t right = FFMIN(end_frame, interval->end_frame);
        if (left >= right) {
            dictate_set_error(errbuf, errbuf_size, "invalid source interval overlap while exporting segments");
            return AVERROR_INVALIDDATA;
        }
        if (!capture->active_open) {
            if (left != interval->start_frame) {
                dictate_set_error(errbuf, errbuf_size, "segment plan started inside an undecoded source frame");
                return AVERROR_INVALIDDATA;
            }
            capture->touched[capture->index] = 1;
            int ret = dictate_open_segment_output(
                &capture->active,
                capture->out_paths[capture->index],
                capture->codec_name,
                capture->channels,
                capture->sample_rate,
                capture->bitrate_kbps,
                capture->codec_has_bitrate,
                capture->sample_fmt_name,
                input_format,
                &capture->gate,
                errbuf,
                errbuf_size
            );
            if (ret < 0) {
                dictate_dispose_segment_output(&capture->active);
                return ret;
            }
            capture->active_open = 1;
        }

        AVFrame *selected = NULL;
        AVFrame *to_convert = (AVFrame *)frame;
        int ret = 0;
        if (left != start_frame || right != end_frame) {
            ret = dictate_alloc_audio_frame(
                &selected,
                frame->format,
                &frame->ch_layout,
                frame->sample_rate,
                (int)(right - left),
                errbuf,
                errbuf_size
            );
            if (ret < 0) return ret;
            ret = av_samples_copy(
                selected->extended_data,
                frame->extended_data,
                0,
                (int)(left - start_frame),
                (int)(right - left),
                frame->ch_layout.nb_channels,
                frame->format
            );
            if (ret < 0) {
                av_frame_free(&selected);
                dictate_set_av_error(errbuf, errbuf_size, "could not select source segment samples", ret);
                return ret;
            }
            to_convert = selected;
        }
        ret = dictate_convert_and_queue_frame(
            capture->active.swr,
            input_format,
            capture->active.enc_ctx,
            capture->active.fifo,
            to_convert,
            errbuf,
            errbuf_size
        );
        av_frame_free(&selected);
        if (ret < 0) return ret;
        ret = dictate_write_fifo_to_encoder(
            capture->active.fifo,
            capture->active.enc_ctx,
            capture->active.ofmt_ctx,
            capture->active.out_stream,
            0,
            &capture->active.next_pts,
            errbuf,
            errbuf_size
        );
        if (ret < 0) return ret;

        if (interval->end_frame <= end_frame) {
            ret = dictate_finish_segment_output(
                &capture->active,
                input_format,
                &capture->gate,
                errbuf,
                errbuf_size
            );
            capture->active_open = 0;
            if (ret < 0) return ret;
            capture->index++;
        } else {
            return 0;
        }
    }
    return 0;
}

static int dictate_validate_segment_export_request(
    const char *in_path,
    const char *const *out_paths,
    const DictateAudioInterval *intervals,
    size_t interval_count,
    int expected_source_rate,
    int64_t expected_source_frames,
    char *errbuf,
    int errbuf_size
) {
    if (!in_path || !*in_path || !out_paths || !intervals || interval_count == 0 ||
        expected_source_rate <= 0 || expected_source_frames <= 0) {
        dictate_set_error(errbuf, errbuf_size, "invalid segment export request");
        return AVERROR(EINVAL);
    }
    int64_t cursor = 0;
    for (size_t n = 0; n < interval_count; n++) {
        if (!out_paths[n] || !*out_paths[n] || strcmp(in_path, out_paths[n]) == 0 ||
            intervals[n].start_frame != cursor ||
            intervals[n].end_frame <= intervals[n].start_frame ||
            intervals[n].end_frame > expected_source_frames) {
            dictate_set_error(errbuf, errbuf_size, "segment plan must be a non-empty contiguous source-frame partition");
            return AVERROR(EINVAL);
        }
        cursor = intervals[n].end_frame;
    }
    if (cursor != expected_source_frames) {
        dictate_set_error(errbuf, errbuf_size, "segment plan does not cover the expected source frames");
        return AVERROR(EINVAL);
    }
    return 0;
}

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
    DictateCancel cancel,
    void *cancel_context,
    char *errbuf,
    int errbuf_size
) {
    int ret = dictate_validate_segment_export_request(
        in_path,
        out_paths,
        intervals,
        interval_count,
        expected_source_rate,
        expected_source_frames,
        errbuf,
        errbuf_size
    );
    if (ret < 0) return ret;
    if (!codec_name || !*codec_name || channels <= 0 || sample_rate <= 0) {
        dictate_set_error(errbuf, errbuf_size, "invalid segment output settings");
        return AVERROR(EINVAL);
    }
    int *touched = av_calloc(interval_count, sizeof(*touched));
    if (!touched) {
        dictate_set_error(errbuf, errbuf_size, "could not allocate segment export state");
        return AVERROR(ENOMEM);
    }
    DictateExportCapture capture = {
        .out_paths = out_paths,
        .intervals = intervals,
        .count = interval_count,
        .touched = touched,
        .expected_source_rate = expected_source_rate,
        .codec_name = codec_name,
        .channels = channels,
        .sample_rate = sample_rate,
        .bitrate_kbps = bitrate_kbps,
        .codec_has_bitrate = codec_has_bitrate,
        .sample_fmt_name = sample_fmt_name,
        .gate = {NULL, 0, 0, 0, 0, cancel, cancel_context},
    };
    int actual_rate = 0;
    int64_t actual_frames = 0;
    ret = dictate_scan_decoded_input(
        in_path,
        debug,
        cancel,
        cancel_context,
        dictate_export_frame,
        &capture,
        &actual_rate,
        &actual_frames,
        errbuf,
        errbuf_size
    );
    if (ret >= 0 && (actual_rate != expected_source_rate || actual_frames != expected_source_frames)) {
        dictate_set_error(errbuf, errbuf_size, "input source rate or frame count differs from segment plan");
        ret = AVERROR_INVALIDDATA;
    }
    if (ret >= 0 && (capture.active_open || capture.index != interval_count)) {
        dictate_set_error(errbuf, errbuf_size, "input ended before every segment interval was exported");
        ret = AVERROR_INVALIDDATA;
    }
    if (capture.active_open) {
        int close_ret = dictate_dispose_segment_output(&capture.active);
        capture.active_open = 0;
        if (ret >= 0 && close_ret < 0) ret = close_ret;
    }
    if (cancel && cancel(cancel_context)) ret = AVERROR_EXIT;
    if (ret < 0) {
        for (size_t n = 0; n < interval_count; n++) {
            if (touched[n]) remove(out_paths[n]);
        }
    }
    av_free(touched);
    return ret;
}
