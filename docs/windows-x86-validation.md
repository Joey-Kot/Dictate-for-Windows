# Windows x86 / x86_64 validation

The 32-bit and 64-bit releases share the same GUI, CLI, audio workflows, and
FFmpeg component list. Separate MinGW toolchains, build trees, and install
prefixes prevent the static codec archives from being mixed between targets.
The default build scripts reject mismatched compilers or existing FFmpeg
libraries before building.

Local Linux validation for this change:

- Both architectures' FFmpeg 9.0.2 and codec dependencies were built from source
  as static libraries. Their GUI and CLI release executables linked successfully.
- PE inspection confirmed `pei-i386` for x86 and `pei-x86-64` for amd64, with no
  dynamic libav, codec, PortAudio, libatomic, GCC, C++, or winpthread dependency.
- Linux workspace tests and Clippy passed, including a native static-libav test
  build. Windows GNU all-target Clippy passed for both architectures.
- Rewrite pointer-queue tests cover full signed 32-bit coordinates, ordering,
  stale generations, and failed message posts.
- The packaged-CLI smoke test was exercised locally with a Linux embedded-libav
  build. It converts a WAV fixture to Opus, sends it to a loopback ASR stub, and
  checks the resulting transcript.

The workflow builds and packages each Windows architecture independently, then
uses a Windows x64 runner to extract both GUI/CLI archives and run the same
conversion smoke test against each CLI. The x86 executable runs under WOW64 in
that job. This workflow has not been run as part of the local validation.

Real 32-bit Windows desktop and microphone validation is still required. Check
startup and tray behavior, recording and device selection, audio formats and
batch transcription, global hotkeys, clipboard/SendInput into both 32-bit and
64-bit applications, and the nonactivating Rewrite menu with scrolling,
outside release, negative monitor coordinates, and mixed DPI. Also verify long
recordings and concurrent uploads within a 32-bit process's address space.
