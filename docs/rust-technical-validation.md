# Rust technical validation record

## Settings control rounded-edge rendering

Settings and Rewrite editor shapes now use a cached Direct2D target and a
premultiplied-alpha DIB, composited at native pixel size into the existing GDI
DC. Text remains on the original GDI path. Dropdowns use a layered child
surface behind their opaque native content, with synchronized position,
visibility, z-order and DPI. Rendering failures restore the GDI fallback.

Checks completed for this change:

- `cargo test -p stt-gui --features native-gui`: all 14 portable GUI tests pass.
- `cargo clippy -p stt-gui --all-targets --target x86_64-pc-windows-gnu --features native-gui -- -D warnings`: passes, including the Windows pixel-test code.
- `cargo build -p stt-gui --target x86_64-pc-windows-gnu --features native-gui`: development executable compiles and links; this is not a production static-libav release build.
- `cargo test -p stt-gui --target x86_64-pc-windows-gnu --features native-gui --no-run`: Windows test executable compiles and links.
- `cargo fmt --all --check` and `git diff --check`: pass.

Two new Windows-only pixel tests cover transparent and partially covered edges,
premultiplied color channels, scratch-surface reuse, and GDI composition with a
negative viewport origin and a clip region. They have **not been executed** in
this Linux environment. On Windows, run
`cargo test -p stt-gui --features native-gui rounded_shape_tests`.

Windows desktop verification remains pending for all Settings pages and the
Rewrite editor at 100%, 125%, 150% and 200% DPI. Check dropdown open/close and
upward placement, hover/selection and keyboard navigation, scrollbar dragging,
padding clicks, overlap with native inputs, page switches, disabled controls,
and settings-window movement/hiding and monitor changes. Compilation and pixel
tests alone do not establish these visual and interaction results.

## Selected-text Rewrite update

See [Rewrite validation](rewrite-validation.md) for this change's automated checks and pending Windows desktop checks. Historical counts and static audio builds below describe earlier changes.

## Microphone selection and native-format capture update

The current WASAPI backend supersedes the PortAudio/WMME and capture-rate
fallback descriptions in the historical records below. See
[microphone selection validation](microphone-selection-validation.md) for the
current automated results and outstanding Windows hardware/GUI checks.

## VAD and shared embedded audio validation

The implementation adds a pinned Earshot 1.2.2 detector, streaming analysis,
source-frame interval gating, shared GUI/CLI conversion and capture-rate fallback.
All 66 Linux tests (59 core, 7 CLI), including native fixtures, pass, as do
format checks and Clippy with warnings denied. Both Windows release
binaries link successfully against the expanded static FFmpeg build; PE import
inspection finds no dynamic PortAudio or libav dependency.

Automated coverage includes configuration migration/bounds, explicit CLI disable,
integer interval mapping/shared padding/5,000 intervals, silence and partial
frames, synthetic speech with pauses, detector isolation, exact stereo sample
selection, cancellation cleanup and out-of-range selection failure. Native fixture
tests decode WAV/PCM, MP3, FLAC, Opus/Ogg, Vorbis/Ogg, AAC/M4A, ALAC/M4A,
WebM, Matroska, WavPack, AC3 and EAC3 with VAD both enabled and disabled.

Planar ALAC interval selection also matches the fully decoded PCM sample for
sample. Output roundtrips cover PCM 16-bit/24-bit/float, AAC, ALAC, FLAC, AC3,
EAC3, Opus, MP3 and Vorbis. Device mocks verify both device-rate and target-rate
fallback; a local HTTP server verifies audio preparation before automatic retry.

The fixed speech fixture is the repository's generated Flite “test” PCM in
`scripts/connectivity_test.pcm.b64`; silence, pauses and stereo sample-index
fixtures are constructed deterministically in tests. Extra codec fixtures can be
generated with `bash scripts/generate-audio-test-fixtures.sh OUTPUT_DIRECTORY`;
that developer-only script requires an FFmpeg executable with Flite.

CI runs `bash scripts/test-embedded-audio.sh` after fetching the Windows
FFmpeg source. It creates a separate Linux libav build and deterministic codec
fixtures under `native-audio-test-tmp`, then runs native tests and Clippy.
Only this test generator requires a system FFmpeg executable.

Native tests require an installed FFmpeg 7.1.1 development build and
`PKG_CONFIG_PATH` pointing to its pkgconfig directory:

```bash
STT_AUDIO_FIXTURES=/absolute/fixture/directory cargo test --workspace --features stt-core/static-libav
cargo clippy --workspace --all-targets --features stt-core/static-libav -- -D warnings
```

This Linux environment cannot establish real Windows 10/11 microphone, UI or
driver behavior. Still required: 48 kHz and fallback devices, USB/Bluetooth/virtual
inputs, long recordings, frequent pauses, Chinese/English/mixed speech, music,
keyboard/fan noise, disabled padding/save validation in all five UI languages,
matching GUI/CLI output and running both release binaries without system FFmpeg.
Synthetic speech tests do not establish natural-speech detection accuracy.

## Optional SendInput channel validation

- Linux: 52 tests pass (46 core, 6 CLI), including Unicode batching, partial/zero
  injection, cancellation, channel selection and configuration overrides.
- Formatting and workspace Clippy with warnings denied pass.
- An isolated Windows-target crate importing the actual core `keyboard.rs`
  passes type-checking with windows 0.61.3, including SendInput and modifier APIs.
- Full Windows cross-check was attempted but blocked by the missing
  `x86_64-w64-mingw32-gcc` compiler required by `ring`. Windows GUI behavior and
  release binaries have not been validated for this change on this Linux host.
  Downloading isolated MinGW packages was rejected by the tool approval policy.

The following automated validation record describes the earlier rewrite.

## Automated validation completed

- `cargo test --workspace`: 38 behavioral tests pass on Linux
  (34 core tests and 4 CLI tests).
- `cargo fmt --all --check` and
  `cargo clippy --workspace --all-targets -- -D warnings` pass.
- `cargo check --workspace --target x86_64-pc-windows-gnu --features
  stt-gui/native-gui`: Win32, Direct2D/DirectWrite, clipboard, hotkey, and
  PortAudio FFI signatures type-check with the GNU Windows target.
- PortAudio v19.7.0 was cross-built using MinGW-w64. Its configuration summary
  reported `WMME=yes` and `WASAPI=no`.
- FFmpeg n7.1.1 and the Ogg, Vorbis, Opus 1.5.2, LAME 3.100, and
  OpenCore AMR 0.1.6 dependencies were cross-built as static MinGW archives.
- The earlier rewrite linked libav only in the GUI; the shared audio implementation
  now links it through core in both release binaries.
- PE inspection reports the CLI as Windows CUI and the GUI as Windows GUI.
  The GUI imports no PortAudio/libav DLL and contains the static libav backend
  marker but no external `ffmpeg.exe` marker. Both binaries now use embedded audio.
- The original GUI import table contained only `keybd_event`; the optional
  Unicode input channel now requires `SendInput` in both CLI and GUI. CI checks both imports.
- CI checks source/binary markers for notifications, external
  FFmpeg in the GUI, and dynamic PortAudio/libav DLL imports.

## Windows hardware validation checklist

These checks cannot be proven on the Ubuntu build host and must be run on real
Windows 10 and Windows 11 systems:

- blocking PortAudio recording on the default device;
- playable WAV output and pause intervals containing no captured samples;
- static libav representative conversions: Ogg/Opus, MP3, FLAC, AAC, PCM;
- RegisterHotKey and WH_KEYBOARD_LL behavior, held-key suppression, and focus;
- Ctrl+V paste into multiple foreground controls, configurable wait intervals,
  and clipboard restoration;
- optional SendInput in GUI and CLI: Notepad, browser, VS Code, terminal and chat
  fields; Chinese, emoji, CR/LF, tabs and long text; preserve clipboard text,
  images and files; modifier release, elevated targets, partial input and cancel;
- SendInput checkbox save/reopen, both delay controls disabled while enabled,
  retained delay values, five languages and DPI layout. These require Windows
  desktop validation; cross-compilation does not prove application compatibility;
- full/minimal sizing, 4 px drag threshold, tray synchronization, taskbar tab;
- matching antialiased, per-pixel-alpha self-drawn rounded corners on Windows 10
  and Windows 11 for both floating and settings windows, with no native
  rectangular border or second DWM corner layer;
- per-monitor high-DPI movement and rendering;
- settings tabs, password token edit, five display languages, save/reload;
- cancel button and Cancel or Retry hotkey abort a blocked ASR request and return to Idle; verify the same hotkey retries the buffered recording while idle in both GUI and CLI;
- busy quit confirmation and bounded shutdown during upload/libav/driver work.

The native GUI dimensions are 222×94 for the full window, 170×46 for minimal
mode, and 760×620 for the settings window.
