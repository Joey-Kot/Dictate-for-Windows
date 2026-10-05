# Microphone selection and device-format capture

## Implemented behavior

- `dictate_core::audio_devices` owns active endpoint enumeration, stable Windows
  endpoint IDs, default-device resolution, format queries and WASAPI shared-mode
  capture. GUI and CLI call the same core implementation.
- `INPUT_DEVICE=""` (including old configurations with no field) follows the
  system default capture endpoint (`eConsole`) at each recording start. A fixed
  endpoint is never resolved by name or enumeration index and never silently
  falls back to a different device. `INPUT_DEVICE_NAME` is display-only metadata.
- Capture prefers `PKEY_AudioEngine_DeviceFormat` if shared-mode
  `IsFormatSupported` returns `S_OK`; otherwise it uses that endpoint's
  `GetMixFormat`. It never negotiates capture from upload settings.
- Temporary WAV storage preserves the sample rate, channel layout and effective
  precision. Integer padding bytes are removed losslessly when needed: FFmpeg 7
  otherwise treats 24 valid bits in a 32-bit WAV container as Cool Edit float.
  Actual 32-bit PCM and float32 remain 32-bit. VAD still analyzes 16 kHz mono;
  the final encoder reads the original captured audio.
- Pause stops the stream. Resume resets buffered data before restarting. COM
  interfaces and their apartment stay on the recorder thread. Enumeration uses
  independent apartments and does not initialize/terminate the active stream.
- Audio's first GUI control is the microphone dropdown. It refreshes on settings
  open and dropdown open using a worker and a channel; closing the window drops
  the receiver without leaving posted result pointers. Offline selections remain
  visible. Settings save/cancel rules are unchanged.
- Language and microphone options share a rounded dark panel with teal selection
  and hover highlights. Hover transitions invalidate only affected rows; each row
  is composed offscreen before display to avoid intermediate paint flicker.
- CLI `--list-input-devices` exits before configuration lookup or ASR setup.
  `--input-device ID` overrides the loaded config for this run only;
  `--input-device default` explicitly follows the system default. File mode does
  not open a microphone.

## Automated validation

Executed on Linux with Windows GNU cross-compilation:

- Workspace tests with embedded libav and all existing audio fixtures: 75 passed
  (67 core, 8 CLI).
- Tests cover old configuration loading and offline selection persistence,
  CLI override/default precedence, fixed-device failure without fallback and
  retry after reconnection, startup/read failure cleanup, pause/cancel behavior,
  malformed formats, frame alignment, WAV padding and lossless 24-in-32 packing.
- End-to-end capture-WAV/VAD/encoding cases use 44.1/48 kHz stereo, 16/24/32-bit
  integer, 24 valid bits in 32-bit containers and float32. With VAD on and off,
  outputs are checked for 24 kHz mono 24-bit PCM and correct duration/trimming.
  These use actual embedded FFmpeg and Earshot, not a mocked converter.
- Windows-target compilation and Clippy with warnings denied include GUI,
  CLI and core. Windows release CLI and GUI link successfully. PE import
  inspection finds only system DLL dependencies, with no PortAudio/libav DLLs.
- The GUI embeds a Common Controls v6 manifest for its subclass helpers. The
  rebuilt EXE was checked for the manifest resource and v6 dependency declaration;
  the release workflow checks these as well.

The extra native validation build is isolated in `microphone-audio-tmp/`, with
its own virtual environment, FFmpeg installation prefix and Cargo target
directory; no system packages were installed. The standard CI audio script also
enables `pcm_s32le` decoding for these tests.

## Windows manual validation

The user reported that manual testing passed after the dropdown startup and
hover-flicker fixes. This is user-reported Windows validation; the build environment
itself had no interactive Windows desktop. The report did not enumerate individual
devices, drivers, or display configurations.

The following checklist is retained for hardware and GUI regression testing:

1. GUI and CLI list built-in, USB, Bluetooth and virtual recording endpoints;
   identical names remain distinguishable and saved IDs survive list reordering.
2. Change the system default between recordings: default mode follows it, while
   a fixed selection stays fixed. Confirm the recorded signal is from that mic.
3. Unplug before/during recording, reconnect, and disable a selected endpoint.
   Confirm explicit errors, no unexpected microphone switch, cleanup and recovery.
4. Check device-default formats and the engine-format fallback against Windows
   settings and `RECORD_DEBUG`, including 44.1/48 kHz and float/integer devices.
5. Verify pause/resume excludes paused audio and does not replay old packets;
   stop, cancel and shutdown release the device.
6. Check dropdown rendering, scrolling, long/same names, keyboard selection,
   Escape, outside-click dismissal, five languages and Windows display scaling.
   Refresh or close settings while enumeration runs and while recording.
7. Verify save/cancel and next-recording application, then load the GUI config
   from CLI and confirm the same endpoint. Check command-line overrides do not
   modify that file; list-only mode needs no API configuration.
