use super::*;
use base64::Engine;

fn wav(path: &Path, rate: u32, channels: u16, samples: &[i16]) {
    let mut writer = hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .unwrap();
    for &s in samples {
        writer.write_sample(s).unwrap();
    }
    writer.finalize().unwrap();
}

fn bridge(
    input: &Path,
    output: &Path,
    intervals: &[Interval],
    enabled: bool,
    cancel: Cancel,
    context: *mut c_void,
) -> i32 {
    let input = string(input.to_string_lossy().as_bytes()).unwrap();
    let output = string(output.to_string_lossy().as_bytes()).unwrap();
    let mut error = [0; 4096];
    let result = unsafe {
        dictate_ffmpeg_convert(
            input.as_ptr(),
            output.as_ptr(),
            c"pcm_s16le".as_ptr(),
            2,
            48000,
            0,
            0,
            c"s16".as_ptr(),
            0,
            intervals.as_ptr(),
            intervals.len(),
            i32::from(enabled),
            cancel,
            context,
            None,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            error.as_mut_ptr(),
            4096,
        )
    };
    if result < 0 {
        eprintln!(
            "bridge: {}",
            unsafe { CStr::from_ptr(error.as_ptr()) }.to_string_lossy()
        );
    }
    result
}

#[test]
fn exact_stereo_gate_thousands_of_intervals_and_disabled_path() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.wav");
    let output = dir.path().join("out.wav");
    let data: Vec<i16> = (0..48000)
        .flat_map(|n| [(n % 30000) as i16, -((n % 30000) as i16)])
        .collect();
    wav(&input, 48000, 2, &data);
    let token = CancellationToken::new();
    let context = (&token as *const CancellationToken).cast_mut().cast();
    for ranges in [
        vec![Interval {
            start_frame: 1,
            end_frame: 47999,
        }],
        (0..5000)
            .map(|n| Interval {
                start_frame: n * 9 + 1,
                end_frame: n * 9 + 4,
            })
            .collect(),
    ] {
        assert_eq!(bridge(&input, &output, &ranges, true, canceled, context), 0);
        let got: Vec<i16> = hound::WavReader::open(&output)
            .unwrap()
            .samples()
            .map(Result::unwrap)
            .collect();
        let expected: Vec<i16> = ranges
            .iter()
            .flat_map(|i| {
                data[i.start_frame as usize * 2..i.end_frame as usize * 2]
                    .iter()
                    .copied()
            })
            .collect();
        assert_eq!(got, expected);
    }
    assert_eq!(bridge(&input, &output, &[], false, canceled, context), 0);
    let got: Vec<i16> = hound::WavReader::open(&output)
        .unwrap()
        .samples()
        .map(Result::unwrap)
        .collect();
    assert_eq!(got, data);
}

#[test]
fn cancellation_and_truncated_selection_remove_output() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.wav");
    let output = dir.path().join("out.wav");
    wav(&input, 48000, 2, &vec![1; 48000 * 10]);
    extern "C" fn cancel_after(context: *mut c_void) -> i32 {
        let count = unsafe { &mut *context.cast::<usize>() };
        *count += 1;
        i32::from(*count > 30)
    }
    let mut calls = 0usize;
    assert!(
        bridge(
            &input,
            &output,
            &[],
            false,
            cancel_after,
            (&mut calls as *mut usize).cast()
        ) < 0
    );
    assert!(!output.exists());
    let token = CancellationToken::new();
    assert!(
        bridge(
            &input,
            &output,
            &[Interval {
                start_frame: 0,
                end_frame: 48000 * 20
            }],
            true,
            canceled,
            (&token as *const CancellationToken).cast_mut().cast()
        ) < 0
    );
    assert!(!output.exists());
}

fn speech() -> Vec<i16> {
    let encoded = include_str!("../../../scripts/connectivity_test.pcm.b64");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded.split_whitespace().collect::<String>())
        .unwrap();
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i16::from_le_bytes(*b))
        .collect()
}

#[tokio::test]
async fn native_capture_formats_reencode_with_and_without_vad() {
    use crate::audio_devices::test_capture_format;
    use crate::capture_wav::CaptureWav;
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("capture.wav");
    let output = dir.path().join("upload.wav");
    let mut source = vec![0_i16; 16000];
    source.extend(speech());
    source.extend(vec![0; 16000]);
    for rate in [44100_u32, 48000] {
        for (bits, valid, float) in [
            (16, 16, false),
            (24, 24, false),
            (32, 24, false),
            (32, 32, false),
            (32, 32, true),
        ] {
            let format = test_capture_format(rate, 2, bits, valid, float);
            let frame_count = source.len() * rate as usize / 16000;
            let mut bytes = Vec::new();
            for frame in 0..frame_count {
                let value = source[frame * 16000 / rate as usize];
                let sample = if float {
                    (f32::from(value) / 32768.0).to_le_bytes().to_vec()
                } else {
                    ((i32::from(value) << 16).to_le_bytes()[4 - usize::from(bits / 8)..]).to_vec()
                };
                bytes.extend_from_slice(&sample);
                bytes.extend_from_slice(&sample);
            }
            let mut writer = CaptureWav::create(&input, &format).unwrap();
            writer.write(&bytes).unwrap();
            writer.finalize().unwrap();
            for enable_vad in [false, true] {
                // Explicit PCM codecs determine bit depth; "pcm" means pcm_s16le.
                let config = Config {
                    enable_vad,
                    codecs: "pcm_s24le".into(),
                    container: "wav".into(),
                    sampling_rate: 24000,
                    sampling_rate_depth: 24,
                    channels: 1,
                    ..Default::default()
                };
                EmbeddedFfmpegConverter
                    .convert(
                        &CancellationToken::new(),
                        &config,
                        &input,
                        &output,
                        rate as i32,
                    )
                    .await
                    .unwrap_or_else(|error| {
                        panic!("{rate} Hz {bits}/{valid} float={float} VAD={enable_vad}: {error}")
                    });
                let reader = hound::WavReader::open(&output).unwrap();
                assert_eq!(reader.spec().sample_rate, 24000);
                assert_eq!(reader.spec().channels, 1);
                assert_eq!(reader.spec().bits_per_sample, 24);
                let full_frames = source.len() as u32 * 24000 / 16000;
                if enable_vad {
                    assert!(reader.duration() < full_frames - 24000);
                    assert!(reader.duration() > 2400);
                } else {
                    assert!(reader.duration().abs_diff(full_frames) <= 2);
                }
            }
        }
    }
}

#[tokio::test]
async fn speech_silence_tail_stereo_and_non48k() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.wav");
    let output = dir.path().join("out.wav");
    let converter = EmbeddedFfmpegConverter;
    let config = Config {
        enable_vad: true,
        codecs: "pcm".into(),
        container: "wav".into(),
        ..Default::default()
    };
    let token = CancellationToken::new();
    wav(&input, 44100, 2, &vec![0; 44100 * 2 + 34]);
    assert!(matches!(
        converter
            .convert(&token, &config, &input, &output, 44100)
            .await,
        Err(ConvertError::NoSpeech)
    ));
    assert!(!output.exists());
    let mut data = vec![0; 16000];
    data.extend(speech());
    data.extend(vec![0; 32000]);
    data.extend(speech());
    data.extend(vec![0; 16017]);
    wav(&input, 16000, 1, &data);
    converter
        .convert(&token, &config, &input, &output, 16000)
        .await
        .unwrap();
    let reader = hound::WavReader::open(&output).unwrap();
    assert!(reader.duration() > 1600);
    assert!(reader.duration() < (data.len() - 16000) as u32);
    assert_eq!(reader.spec().sample_rate, 16000);
    // Reusing the converter must not retain the previous detector state.
    wav(&input, 16000, 1, &[0; 257]);
    assert!(matches!(
        converter
            .convert(
                &token,
                &config,
                &input,
                &dir.path().join("silence.wav"),
                16000
            )
            .await,
        Err(ConvertError::NoSpeech)
    ));
}

#[tokio::test]
async fn midstream_format_changes_fail_without_partial_output() {
    let Ok(folder) = std::env::var("DICTATE_AUDIO_FIXTURES") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let first = std::fs::read(Path::new(&folder).join("aac-stereo-48k.aac")).unwrap();
    for tail in ["aac-stereo-24k.aac", "aac-mono-48k.aac"] {
        let mut data = first.clone();
        data.extend(std::fs::read(Path::new(&folder).join(tail)).unwrap());
        let input = dir.path().join("changing.aac");
        std::fs::write(&input, data).unwrap();
        for enable_vad in [false, true] {
            let config = Config {
                codecs: "pcm".into(),
                container: "wav".into(),
                enable_vad,
                ..Default::default()
            };
            let output = dir.path().join("output.wav");
            let error = EmbeddedFfmpegConverter
                .convert(&CancellationToken::new(), &config, &input, &output, 48000)
                .await
                .expect_err("changing input format must fail");
            assert!(
                matches!(error, ConvertError::Failed { ref message }
                    if message.contains("input audio format changed during decoding")),
                "{tail}, VAD={enable_vad}: {error}"
            );
            assert!(!output.exists(), "partial output: {tail}, VAD={enable_vad}");
        }
    }
}

#[tokio::test]
async fn supported_input_fixtures() {
    let Ok(folder) = std::env::var("DICTATE_AUDIO_FIXTURES") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        codecs: "pcm".into(),
        container: "wav".into(),
        ..Default::default()
    };
    for filename in [
        "pcm.wav",
        "audio.mp3",
        "audio.flac",
        "opus.ogg",
        "vorbis.ogg",
        "aac.m4a",
        "alac.m4a",
        "audio.webm",
        "audio.mka",
        "audio.wv",
        "audio.ac3",
        "audio.eac3",
    ] {
        let input = Path::new(&folder).join(filename);
        let output = dir.path().join("decoded.wav");
        EmbeddedFfmpegConverter
            .convert(&CancellationToken::new(), &config, &input, &output, 48000)
            .await
            .unwrap_or_else(|e| panic!("{filename}: {e}"));
        let reader = hound::WavReader::open(&output).unwrap();
        assert!(reader.duration() > 1000, "{filename}");
        let vad_config = Config {
            enable_vad: true,
            ..config.clone()
        };
        EmbeddedFfmpegConverter
            .convert(
                &CancellationToken::new(),
                &vad_config,
                &input,
                &output,
                48000,
            )
            .await
            .unwrap_or_else(|e| panic!("VAD {filename}: {e}"));
    }
    // ALAC decodes to planar PCM: compare gated frames against the fully
    // decoded source so sample format and channel offsets are both exercised.
    let input = Path::new(&folder).join("alac.m4a");
    let full = dir.path().join("full.wav");
    let cut = dir.path().join("cut.wav");
    let token = CancellationToken::new();
    let context = (&token as *const CancellationToken).cast_mut().cast();
    assert_eq!(bridge(&input, &full, &[], false, canceled, context), 0);
    let ranges = [
        Interval {
            start_frame: 1,
            end_frame: 257,
        },
        Interval {
            start_frame: 5000,
            end_frame: 9001,
        },
    ];
    assert_eq!(bridge(&input, &cut, &ranges, true, canceled, context), 0);
    let source: Vec<i16> = hound::WavReader::open(&full)
        .unwrap()
        .samples()
        .map(Result::unwrap)
        .collect();
    let actual: Vec<i16> = hound::WavReader::open(&cut)
        .unwrap()
        .samples()
        .map(Result::unwrap)
        .collect();
    let expected: Vec<i16> = ranges
        .iter()
        .flat_map(|i| {
            source[i.start_frame as usize * 2..i.end_frame as usize * 2]
                .iter()
                .copied()
        })
        .collect();
    assert_eq!(actual, expected);
}

#[tokio::test]
async fn native_output_encoders_roundtrip_and_invalid_input() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("speech.wav");
    wav(&input, 16000, 1, &speech());
    let token = CancellationToken::new();
    let converter = EmbeddedFfmpegConverter;
    for (codec, extension) in [
        ("pcm", "wav"),
        ("pcm_s24le", "wav"),
        ("pcm_f32le", "wav"),
        ("opus", "ogg"),
        ("mp3", "mp3"),
        ("vorbis", "ogg"),
        ("aac", "m4a"),
        ("alac", "m4a"),
        ("flac", "flac"),
        ("ac3", "ac3"),
        ("eac3", "eac3"),
    ] {
        let output = dir.path().join(format!("encoded.{extension}"));
        let config = Config {
            enable_vad: true,
            codecs: codec.into(),
            container: extension.into(),
            sampling_rate: 48000,
            bit_rate: 128,
            ..Default::default()
        };
        converter
            .convert(&token, &config, &input, &output, 16000)
            .await
            .unwrap_or_else(|e| panic!("{codec}: {e}"));
        let decoded = dir.path().join("decoded.wav");
        let config = Config {
            codecs: "pcm".into(),
            container: "wav".into(),
            ..Default::default()
        };
        converter
            .convert(&token, &config, &output, &decoded, 48000)
            .await
            .unwrap();
        assert!(hound::WavReader::open(&decoded).unwrap().duration() > 1000);
    }
    let bad = dir.path().join("bad.wav");
    std::fs::write(&bad, b"not audio").unwrap();
    let output = dir.path().join("bad-output.wav");
    assert!(
        converter
            .convert(&token, &Config::default(), &bad, &output, 16000)
            .await
            .is_err()
    );
    assert!(!output.exists());
}

fn export_segments_bridge(
    input: &Path,
    outputs: &[&Path],
    intervals: &[Interval],
    expected_source_rate: i32,
    expected_source_frames: i64,
) -> i32 {
    let input = string(input.to_string_lossy().as_bytes()).unwrap();
    let output_strings: Vec<_> = outputs
        .iter()
        .map(|path| string(path.to_string_lossy().as_bytes()).unwrap())
        .collect();
    let output_paths: Vec<_> = output_strings.iter().map(|path| path.as_ptr()).collect();
    let token = CancellationToken::new();
    let context = (&token as *const CancellationToken).cast_mut().cast();
    let mut error = [0; 4096];
    let result = unsafe {
        dictate_ffmpeg_export_segments(
            input.as_ptr(),
            output_paths.as_ptr(),
            intervals.as_ptr(),
            intervals.len(),
            c"pcm_s16le".as_ptr(),
            2,
            expected_source_rate,
            0,
            0,
            c"s16".as_ptr(),
            expected_source_rate,
            expected_source_frames,
            0,
            canceled,
            context,
            error.as_mut_ptr(),
            4096,
        )
    };
    if result < 0 {
        eprintln!(
            "export segments bridge: {}",
            unsafe { CStr::from_ptr(error.as_ptr()) }.to_string_lossy()
        );
    }
    result
}

#[tokio::test]
async fn native_segmented_analysis_marks_short_complete_silence_as_no_speech() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("short-silence.wav");
    let source_frames = 1_600_u64; // 100 ms at 16 kHz, below the 700 ms pause setting.
    wav(&input, 16_000, 1, &vec![0; source_frames as usize]);

    let analysis = EmbeddedFfmpegConverter
        .analyze_segments(
            &CancellationToken::new(),
            &Config::default(),
            &input,
            700,
        )
        .await
        .unwrap();

    assert_eq!(analysis.source_rate, 16_000);
    assert_eq!(analysis.source_frames, source_frames);
    assert_eq!(
        analysis.silence_intervals,
        vec![SourceFrameInterval {
            start_frame: 0,
            end_frame: source_frames,
        }]
    );
}

#[test]
fn native_segmented_pcm_exports_preserve_boundaries_and_clean_up_the_batch() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.wav");
    let source_frames = 4_877_i64;
    let data: Vec<i16> = (0..source_frames)
        .flat_map(|frame| {
            let left = ((frame * 31 % 30_000) - 15_000) as i16;
            let right = ((frame * 47 % 30_000) - 15_000) as i16;
            [left, right]
        })
        .collect();
    wav(&input, 48_000, 2, &data);

    let first = dir.path().join("first.wav");
    let second = dir.path().join("second.wav");
    let third = dir.path().join("third.wav");
    let outputs = [first.as_path(), second.as_path(), third.as_path()];
    let intervals = [
        Interval {
            start_frame: 0,
            end_frame: 137,
        },
        Interval {
            start_frame: 137,
            end_frame: 2_111,
        },
        Interval {
            start_frame: 2_111,
            end_frame: source_frames,
        },
    ];
    assert_eq!(
        export_segments_bridge(&input, &outputs, &intervals, 48_000, source_frames),
        0
    );
    let reconstructed: Vec<i16> = outputs
        .iter()
        .flat_map(|path| {
            hound::WavReader::open(path)
                .unwrap()
                .samples()
                .map(Result::unwrap)
                .collect::<Vec<i16>>()
        })
        .collect();
    assert_eq!(reconstructed, data);

    // The bridge discovers this plan/source mismatch only after it has
    // written a segment.  Its own failure path must still remove that file,
    // rather than relying on the Rust wrapper's defensive cleanup.
    let mismatched = dir.path().join("mismatched.wav");
    let mismatched_intervals = [Interval {
        start_frame: 0,
        end_frame: source_frames - 1,
    }];
    assert!(
        export_segments_bridge(
            &input,
            &[mismatched.as_path()],
            &mismatched_intervals,
            48_000,
            source_frames - 1,
        ) < 0
    );
    assert!(!mismatched.exists());
}

#[tokio::test]
async fn native_segmented_encoded_outputs_are_independently_decodable() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("speech.wav");
    let samples = speech();
    wav(&input, 16_000, 1, &samples);
    let intervals = [
        SourceFrameInterval {
            start_frame: 0,
            end_frame: 3_111,
        },
        SourceFrameInterval {
            start_frame: 3_111,
            end_frame: 8_333,
        },
        SourceFrameInterval {
            start_frame: 8_333,
            end_frame: samples.len() as u64,
        },
    ];
    let converter = EmbeddedFfmpegConverter;
    let token = CancellationToken::new();
    for (codec, extension) in [("opus", "ogg"), ("mp3", "mp3"), ("aac", "m4a")] {
        let output_dir = dir.path().join(codec);
        std::fs::create_dir(&output_dir).unwrap();
        let outputs: Vec<_> = (0..intervals.len())
            .map(|index| output_dir.join(format!("segment-{index}.{extension}")))
            .collect();
        let config = Config {
            codecs: codec.into(),
            container: extension.into(),
            sampling_rate: 48_000,
            bit_rate: 128,
            ..Default::default()
        };
        converter
            .export_segments(
                &token,
                &config,
                &input,
                &outputs,
                &intervals,
                16_000,
                samples.len() as u64,
            )
            .await
            .unwrap_or_else(|error| panic!("{codec}: {error}"));
        for (index, output) in outputs.iter().enumerate() {
            let decoded = output_dir.join(format!("decoded-{index}.wav"));
            converter
                .convert(
                    &token,
                    &Config {
                        codecs: "pcm".into(),
                        container: "wav".into(),
                        ..Default::default()
                    },
                    output,
                    &decoded,
                    48_000,
                )
                .await
                .unwrap_or_else(|error| panic!("{codec} segment {index}: {error}"));
            assert!(
                hound::WavReader::open(&decoded).unwrap().duration() > 100,
                "{codec} segment {index} was empty"
            );
        }
    }
}
