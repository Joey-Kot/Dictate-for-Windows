# Dictate for Windows compatibility contract

This document records the behavior preserved by the Rust implementation. The
release binaries are `dictate-cli.exe` and `Dictate.exe`.

## Deliberately removed behavior

The Rust implementation removes these former public features:

- `beeep` and all Windows notifications;
- the `NOTIFICATION` configuration field when serializing or saving;
- the `--notification` and `--request-failed-notification` CLI arguments;
- the optional `REQUEST_FAILED_NOTIFICATION` configuration field.

The legacy `TEXT_PATH` dot-path syntax and automatic field fallback have also
been replaced by standard JSONPath with exactly-one-result validation.

Old JSON files containing these fields remain readable because unknown fields
are ignored. Interactive audio failure placeholders are now unconditional after
an eligible terminal request failure; they are not a notification setting.

## Configuration

Configuration precedence is CLI overrides, then the selected JSON file, then
defaults. Missing fields receive defaults and unknown fields are ignored.

| JSON field | Default | Validation / behavior |
|---|---:|---|
| `API_ENDPOINT` | `""` | Required at upload time |
| `TOKEN` | `""` | Sent as Bearer token when non-empty |
| `MODEL` | `""` | Multipart field only when non-empty |
| `LANGUAGE` | `""` | Multipart field only when non-empty |
| `PROMPT` | `""` | Multipart field only when non-empty |
| `TEXT_PATH` | `"$.text"` | Standard JSONPath selecting exactly one scalar; no fallback |
| `ExtraConfig` | `""` | Blank or a JSON object; recursive merge before multipart serialization |
| `REWRITE` | Empty API fields and prompt list, `custom_openai_compatible` provider, menu disabled | Shared by GUI and CLI hotkey mode; independent text API, optional per-prompt hotkeys, and GUI-only menu |
| `OPACITY` | `1.0` | GUI floating-window opacity; `0.10`–`1.00` in `0.01` steps, where `1.0` is fully opaque |
| `WINDOW_SCALE` | `1.0` | GUI floating-window scale; `0.3`–`2.0` in `0.1` steps, shared by full and minimal modes |
| `INPUT_DEVICE` | `""` | Stable Windows capture endpoint ID; empty follows the system default |
| `INPUT_DEVICE_NAME` | `""` | Display-only cached name, not used for device resolution |
| `CHANNELS` | `1` | Inclusive range 1–8; output channels only |
| `SAMPLING_RATE` | `16000` | Final upload rate, greater than zero |
| `ENABLE_VAD` | `false` | Shared by GUI, CLI recording and CLI file mode |
| `VAD_PADDING_MS` | `100` | Integer 0–1000 milliseconds; validated even while VAD is off |
| `VAD_START_THRESHOLD` | `0.6` | Range 0.5–1.0 (inclusive); validated even while VAD is off |
| `SAMPLING_RATE_DEPTH` | `16` | 8, 16, 24, or 32 |
| `BIT_RATE` | `128` | Greater than zero |
| `CODECS` | `"opus"` | Existing alias list, case-insensitive |
| `CONTAINER` | `"opus"` | Existing container list, case-insensitive |
| `REQUEST_TIMEOUT` | `60` | Seconds; positive values set the client timeout |
| `MAX_RETRY` | `3` | Attempt limit, including the first request |
| `RETRY_BASE_DELAY` | `0.5` | Seconds, doubled after each failure |
| `ENABLE_HTTP2` | `true` | Explicit HTTP/2 control |
| `VERIFY_SSL` | `true` | Explicit TLS certificate validation control |
| `HOTKEY_HOOK` | `true` | Low-level hook when true, RegisterHotKey otherwise |
| `START_KEY` | `"ctrl+alt+q"` | Start/stop recording |
| `PAUSE_KEY` | `"ctrl+alt+s"` | Pause/resume |
| `CANCEL_OR_RETRY_KEY` | `"alt+esc"` | Cancel recording/request, or retry the buffered recording while idle |
| `CLIPBOARD_WRITE_DELAY` | `80` | Milliseconds before Ctrl+V after writing output, or before Ctrl+C after clearing the clipboard for Rewrite input |
| `CLIPBOARD_RESTORE_DELAY` | `120` | Milliseconds before restoring the clipboard after Ctrl+V output or reading Rewrite input |
| `CACHE_DIR` | `""` | Empty falls back to current directory |
| `KEEP_CACHE` | `false` | Effective only with a non-empty usable cache dir |
| debug flags | existing values | FFmpeg false, record false, hotkey true, upload false |

The Rust CLI uses only standard long options. Boolean options require an
explicit `true` or `false`. `--rate` is a hidden-compatible alias for
`--sampling-rate`; old single-dash long forms are not accepted. Long help is
organized into General, Audio API, Audio Record, Network, Audio Hotkeys, Cache, and Debug groups. Rewrite configuration is read from JSON; there are no Rewrite CLI flags.

## TEXT_PATH

The configured path uses standard JSONPath through `serde_json_path`, for example
`$.results[0].alternatives[0].transcript`. The default is `$.text`; legacy paths
without the root identifier are not supported. Configuration validation rejects
empty or malformed paths before any request, and each ASR client reuses its parsed
path. Queries must match exactly one node. String, number, and boolean results
are converted to text; objects and arrays are errors. An explicit null or an
empty/whitespace-only transcript is a successful empty recognition and produces
no paste, automatic retry, or failure placeholder in GUI/hotkey mode.

Invalid JSON responses, zero matches, multiple matches (with their count), and
unsupported value types produce explicit errors without fallback or automatic
upload retries. The original HTTP 200 response remains available for caching,
even if it is not valid JSON. CLI file mode returns an error without writing the
transcription file. See the README for selector examples and result semantics.

## HTTP and multipart

- Each retry reopens the file and reconstructs the multipart body.
- The file field is `file`; its uploaded filename is the local basename.
- `model`, `language`, and `prompt` are omitted when empty.
- `ExtraConfig` recursively merges objects, replaces arrays and scalar values, and deletes null object members at any depth, including inside arrays. Null array elements are retained. The binary `file` field is reserved.
- Nested extra values are serialized as compact JSON strings.
- User-Agent is `dictate-client/1.0`.
- Only HTTP 200 succeeds; the original response body is retained.
- Requests and exponential retry waits are cancellable.
- System proxies, redirects, and automatic compression are disabled.
- HTTP/2 and TLS verification follow their explicit settings.

## Selected-text Rewrite

The Rewrite API page follows Audio Record and configures a Provider, Base URL,
API Key, Model, ordered prompts and an optional native menu. Each prompt has a
persistent ID, title, icon, content, optional JSON overrides and an optional
shortcut. An empty shortcut is valid and is not registered. Its optional
`provider` defaults to null (Same as Main Provider), including when missing from
older configurations. Null selects the entire main Rewrite API and ignores the
prompt's `base_url`, `api_key` and `model` while retaining those values. Selecting
a provider uses all four prompt API fields as a unit, even if the provider
matches the main one; empty fields never inherit from the main configuration.
This resolution is shared by GUI and CLI hotkey mode.

API fields may remain incomplete when saving. Actual execution validates the
effective URL and API key and checks the final model after merging the prompt's
Extra config; Extra config may supply or replace the body model but does not
configure the provider, URL or authentication key. Request routing, authentication
and response extraction all use the selected provider.

Add/edit/delete/reorder remain drafts until Settings Save. Editor Cancel
discards its changes; Settings Cancel discards all drafts. The shared Network
and Audio Hotkeys output settings apply to Rewrite. Main and per-prompt
connectivity probes use only the selected API fields and shared network draft,
send fixed instructions and input once, and never read or deliver selected text.
They do not validate or use the draft title, prompt, hotkey or Extra config, so
the API Model field must be set even if a normal prompt supplies it through
Extra config. Probes share the runtime task lock and public cancel action.

`REWRITE.menu_enabled` defaults to false and `menu_key` defaults to an empty
string. Older configurations keep their prompt shortcuts and use the default
icon. Disabling the menu preserves its key without registering it or including
it in active-key conflicts. An enabled menu requires a valid nonempty key at
Settings Save; prompt editor validation permits this temporary missing-key state
so prompts remain editable. The menu registration participates in the same
replacement and rollback transaction as other shortcuts. CLI accepts these
configuration fields and optional prompt keys but never registers the GUI menu
action.

Prompt `icon` is `{"kind":"default"}`, `{"kind":"builtin","value":"..."}`,
or `{"kind":"custom","value":"icons/<SHA256>.<ext>"}`. The 25 built-ins use
user-supplied two-color SVGs embedded in the executable. Their `#1E88E5` and
`#64B5F6` colors remain fixed in both themes. Visible artwork bounds are
normalized before proportional scaling and centering; custom-image behavior
is unchanged. Default uses the same double-chat-bubble artwork as `chat`.

The 25 serialized values match their artwork: `chat`, `messages`, `translate`,
`eye`, `feather`, `document`, `shield`, `pie_chart`, `tags`, `text_card`, `box`,
`flask`, `globe`, `cookie`, `window`, `plug`, `settings`, `layers`, `key`,
`network`, `hierarchy`, `bar_chart`, `microphone`, `fingerprint`, and `layout`.
Former icon IDs have no migration mappings or aliases. Invalid data uses the
same generic Default fallback as other damaged or missing icons.

Missing or invalid icon data falls back without rejecting other prompts.
Managed references are relative to the configuration directory; traversal and
symlink asset paths are rejected. PNG/JPEG/SVG imports are bounded to 4 MiB and
4096 pixels per dimension, with a 64 MiB raster decode allocation limit. SVG
supports vector artwork, installed-font text and validated embedded PNG/JPEG
images with the same limits. It never loads external images or fonts; nested
SVG images and other embedded image types are disabled.
Custom images preserve original colors, alpha and aspect ratio. Decode/scale
results are cached by reference, physical size and theme without recoloring
custom assets.

Imported bytes remain in memory through the editor and Settings drafts. Editor
Save merges its imports into the parent draft; either Cancel drops that level's
imports. Main Save writes only referenced, content-addressed assets and commits
them only after configuration saving succeeds. A failed save removes newly
created assets and preserves prior resources; existing assets are never
overwritten or deleted by a later import.

The prompt editor places a compact icon preview and name above Title, with
adjacent Choose icon and Import image buttons. Choose icon opens a 5-by-5
popup containing only the 25 built-ins; selecting an icon closes the popup.
Arrow keys move within the grid; Tab/Shift+Tab traverse its items and leave at
the boundary. Enter/Space select and Esc closes without selecting. Outside
clicks, editor deactivation, scrolling and DPI changes dismiss the popup.
The scrolling body and fixed Save/Cancel footer are retained. It retains Provider selection, with
Same as Main Provider first and all available provider choices after it. A separate provider reveals the three
API fields and a footer Test connectivity button. Same as Main Provider hides
those fields, the test button and its result. Editing an API field, choosing a
provider or closing the editor cancels its probe and discards stale results.
Save and Test are disabled during a probe; Cancel remains available. The editor
uses the existing dropdown, rounded input, button and scrollbar styles.

Provider wire formats are selected by the configured provider: OpenAI, Azure and
Volcengine Responses use the Responses API; Google Gemini and Vertex AI use the
Gemini API; Anthropic uses Messages; Qwen Text and Qwen Multimodal use their
native DashScope routes; MiniMax uses ChatCompletion V2; the remaining providers
use Chat Completions. API endpoints are completed from the Base URL. Per-prompt
extras merge into the JSON request before validating the final model. Google puts
this model in the URL. Responses may be JSON or SSE; only a complete, nonblank
final text result reaches shared output. No reasoning blocks or partial SSE
output are delivered. Responses are bounded to 2 MiB. Network errors, HTTP
408/429/5xx and corresponding service errors retry up to the shared total
attempt count; configuration, malformed/empty responses and other HTTP errors
fail immediately.

Shared core input acquisition always uses the target application's Copy command:
materialize a clipboard backup, clear the clipboard, wait `CLIPBOARD_WRITE_DELAY`,
inject `Ctrl+C`, read fresh nonblank Unicode text, wait `CLIPBOARD_RESTORE_DELAY`,
then restore the backup before requesting Rewrite. The two delays share the
Audio Hotkeys settings and defaults of 80 ms and 120 ms, regardless of
`USE_SENDINPUT`, which controls output only. No UI Automation selection interface
is used. Shortcut modifiers and `C` must be released within two seconds; copy
polling allows three seconds. Native clipboard calls may exceed these polling
limits. Input is bounded to 1,000,000 UTF-8 bytes. Cancellation or focus changes
during the pre-copy delay restore without copying; after Copy, the configured
restore delay completes even when canceled.

The backup/clear phase uses a null clipboard owner. The native worker has no
window message loop, so retaining an owner window while waiting for Copy would
block the target application's `EmptyClipboard` in `WM_DESTROYCLIPBOARD` and
leave the clipboard locked. Restoration alone uses a temporary owner window for
`SetClipboardData`, closing the clipboard before destroying the window. All
restored formats are materialized and need no delayed-rendering window.

Snapshots preserve supported materializable text, HTML/RTF, image and file-list
formats, up to 64 MiB and 256 formats. Unsupported or unmaterializable formats
fail before the original clipboard is cleared. Cancellation and normal shutdown
wait for the single native worker to attempt restoration; the shared task stays
busy during cleanup. Restore failures remain visible even after cancellation
and prevent a request or output. Missing fresh nonblank text, read errors and
foreground/focused-window changes also prevent a request.

Input follows the target's Copy behavior: editors may copy a current line without
a selection, and this path cannot independently exclude password controls or
prove that copied text was selected. Application compatibility requires Windows
desktop validation. Successful output calls the same `text_input::send_text` as
ASR; there is no additional focus restoration or delivery mode. Failed/canceled
Rewrite never calls output or uses audio failure placeholders, and never
replaces the audio retry buffer. Once delivery begins, the existing output
channel's partial-delivery limitations still apply.

Nonempty Rewrite shortcuts and the enabled Menu key are normalized and checked
against every audio action and other prompt. Both backends require an exact
modifier set and reject identical normalized combinations. Audio, Prompt, and
Menu bindings may share an ordinary key with different modifiers. For example,
`Ctrl+Q`, `Shift+Q`, and `Alt+Q` are independent bindings; `Ctrl+Shift+Q` triggers
only its own binding, if configured. With any Rewrite prompt configured,
RegisterHotKey mode also rejects an exact `Ctrl+C` binding on any audio action or
prompt or enabled menu, preventing interception of the injected copy command. Hook mode ignores
injected events and permits `Ctrl+C` subject to normal conflicts. Both GUI save
paths and recording hints use the same exact-combination conflict rules.
Reload registers shortcuts against
stable prompt IDs, independently of the number of preceding prompts with empty
shortcuts. Recording optional shortcuts supports clearing while preserving
application-hotkey suspension and non-repeating held-key behavior.

When the hook consumes a physical main key with Alt held and Ctrl not held,
it injects one unassigned `VK_E8` down/up pair before dispatching the action.
This prevents the foreground application from seeing a bare Alt tap and moving
focus to its menu bar. Repeat keydowns remain suppressed, injected events bypass
the hook, and physical modifier releases remain untouched. Ctrl+Alt/AltGr and
unmatched chords do not need this mask. No foreground window is stored/restored,
and the Rewrite copy transaction and timeout remain unchanged.

### Native Rewrite menu

The GUI creates a separate nonactivating Win32 popup with a premultiplied
DirectComposition surface. Showing it, clicking rows,
scrolling and dragging its scrollbar must retain the target application's focus
and selection. A menu click closes the popup and releases its capture/listeners
before dispatching the selected stable prompt ID through `try_rewrite()`. There
is no stored target window and no focus restoration; output uses the focus and
selection at delivery time. Existing Rewrite input, cancellation, errors, output
and the non-queuing runtime task lock remain shared with prompt hotkeys.

The menu is positioned from the pointer's screen coordinates at invocation,
using that monitor's DPI and work area, including negative monitor coordinates.
The compact layout uses 248 DIP width, 40 DIP per row, a 22 DIP icon, 14 DIP text,
4 DIP top/bottom padding and 8 DIP corner radius aligned with native DWM
rounding. Seven visible rows occupy 248 by 288 DIP. One to seven entries use
only their required complete rows; longer lists expose at most seven rows and reserve space for a slim
rounded scrollbar. Titles remain single-line with ellipsis. Separators are
inset and omitted after the last entry. An empty list displays status feedback
without opening a panel.

Pressing Menu key again or `Esc` closes the menu. While open, `Esc` must not also
cancel the target application's selection or exit the host. Outside clicks
close it and continue to the target application. Wheel events over the menu
must not scroll the application behind it, regardless of the Windows inactive
window scrolling preference. A scrollbar drag must end after an outside mouse
release; closing and destruction remove temporary input listeners and capture.

The surface follows Windows default app light/dark mode, including while open.
The four supplied visual references are the acceptance standard for blur,
neutral tint, subtle grain, rounded contour, thin edge/highlight, shadow,
colored icons, spacing, separators and scrollbar. No wallpaper or fixed blue
gradient is part of the menu. Text and icons are drawn sharply above the
material. An isolated compatibility module dynamically resolves
`SetWindowCompositionAttribute` from `user32.dll` and requests
`WCA_ACCENT_POLICY=19`, Acrylic state `4`, flags `0`. Its `GradientColor`
supplies the sole tint: light `#F7F7F7` at approximately 70% opacity (`0xB3`
alpha), dark `#202227` at 80% opacity (`0xCC` alpha). The preceding tint
adjustment changed only light opacity; the subsequent icon replacement keeps
these material colors, dimensions and interactions unchanged. The managed system backdrop is explicitly disabled with
`DWMSBT_NONE`, and `DwmExtendFrameIntoClientArea` uses zero margins, retaining
DWM rounding and the outer shadow. Direct2D does not add another full-surface
tint when the accent request succeeds; it paints a solid surface only on
fallback.

The accent policy has no stable public contract. Microsoft's
[SetWindowCompositionAttribute documentation](https://learn.microsoft.com/en-us/windows/win32/dwm/setwindowcompositionattribute)
describes dynamic resolution and recommends `DwmSetWindowAttribute` instead;
the compatibility call is isolated and failure selects a solid fallback.
An accepted request does not prove inactive translucency. The path is attempted
only when Windows 11 rounded-window attributes are available, desktop
composition is active, and transparency is allowed. Windows 10, unsupported
material/API failures, disabled transparency, high contrast and battery saver
use a matching solid fallback. This material change does not alter activation
or focus handling. No activation is performed to obtain material.

The user reported functional success after the `Alt+A` hook fix in their tested
scenario; this is not evidence that every application/input combination was
checked. A comparison screenshot showed a flat opaque gray menu with the old
DWM backdrop path, which failed visual acceptance. The user subsequently
provided actual dark and light Accent screenshots with six rows and no
scrollbar, accepted the dark appearance, and requested reducing the light tint
from 80% to approximately 70% opacity. Dark remains at 80%; the 70% light
revision awaits feedback. These reports do not establish acceptance of all four
visual scenarios. The current artwork-based icon IDs and compact popup selector
have not been verified on Windows; earlier functional and background material
feedback does not establish acceptance of this revision. The icon and selector
changes leave the 70% light and 80% dark material parameters unchanged.
Background Acrylic's inactive solid-color behavior is described in Microsoft's
[Acrylic material documentation](https://learn.microsoft.com/en-us/windows/apps/design/style/acrylic).
Windows desktop functional acceptance and visual acceptance are tracked
separately in [the menu validation record](rewrite-menu-validation.md).

## Cache and filenames

The cache path is made absolute and created when possible. Failure clears the
setting and uses the current directory. Startup removes every file or directory
whose name starts with `RecordTemp_` in the active temporary directory.

Temporary names are `RecordTemp_<16 hex chars>.<ext>`. Recorded WAV and
converted audio are renamed to `audio-YYYY-MM-DD-HH.MM.SS.<ext>` when caching
is enabled. If both are WAV, the converted file receives `_convert`. A response
JSON is saved only after an HTTP-200 upload. Otherwise temporary audio is
removed.

## Keyboard, clipboard, and hotkeys

The default clipboard channel uses `keybd_event` for `Ctrl+V`. The exact v1.1.2 sequence is:

1. virtual-key Ctrl down: `(0x11, 0x91, 0)`;
2. scan-code V down: `(47, 175, KEYEVENTF_SCANCODE)`;
3. virtual-key Ctrl up: `(0x11, 0x91, KEYEVENTF_KEYUP)`;
4. scan-code V up: `(47, 175, KEYEVENTF_KEYUP | KEYEVENTF_SCANCODE)`.

Clipboard transport is `CF_UNICODETEXT`. Opening waits for up to one second
and the open/close transaction stays on one OS thread. The runtime reads the
old text, writes the transcription, waits `CLIPBOARD_WRITE_DELAY` milliseconds,
sends Ctrl+V, waits `CLIPBOARD_RESTORE_DELAY` milliseconds, and unconditionally
tries to restore the old text. The defaults remain 80 ms and 120 ms. Both waits
are cancellable. “Paste sent; clipboard restore failed” remains distinct from
pre-paste failure.

The current optional `USE_SENDINPUT` setting supersedes the original prohibition
on SendInput. It defaults to false, including in older configuration files.
When true, shared core output uses SendInput with KEYEVENTF_UNICODE, without
clipboard access, clipboard delays, fallback, or automatic delivery retries.
This applies to transcriptions, successful Rewrite results, and audio failure
placeholders in GUI and CLI hotkey mode. Rewrite input acquisition still uses clipboard Copy regardless of this
setting. CLI `--use-sendinput <BOOL>` overrides the setting. GUI Audio Hotkeys
exposes a checkbox below Restore delay. `SENDINPUT_BATCH_SIZE` defaults to 128
UTF-16 code units and accepts 1 through 4096; CLI
`--sendinput-batch-size <UNITS>` overrides it. GUI exposes the batch-size field
alongside the checkbox and enables it only while SendInput is selected. Both
delay controls remain editable with SendInput selected because Rewrite input
still uses them; saving temporarily disables the controls. Unicode scalars stay
intact across batches;
line endings normalize to CR, and tabs remain Unicode characters. Partial
delivery and cancellation after delivery report that text may already exist.

Hotkeys reject unknown or repeated modifiers and normalize aliases/casing for
duplicate detection. RegisterHotKey mode uses `MOD_NOREPEAT`, a dedicated
message thread, `WM_QUIT`, and unregisters all bindings. Hook mode uses
`WH_KEYBOARD_LL`, ignores `LLKHF_INJECTED`, reads held modifiers with
`GetAsyncKeyState` and requires an exact Ctrl/Shift/Alt/Win modifier set. It
swallows held-repeat keydown and its matching keyup; changing modifiers during
a swallowed hold does not dispatch another action. `CANCEL_OR_RETRY_KEY` cancels while recording
or uploading or rewriting; when idle with a buffered recording, it retries that recording.

## Shared WASAPI recorder

- Core enumerates active capture endpoints and opens them by stable ID. Empty
  `INPUT_DEVICE` resolves the current system default at each recording start.
- GUI and CLI share the configuration and backend. `--list-input-devices` exits
  before configuration lookup; `--input-device ID|default` overrides this run only.
- A fixed unavailable endpoint fails without switching devices. Device names
  and enumeration indices are never used as identifiers.
- WASAPI shared mode prefers the endpoint's Windows-configured default format;
  if unavailable or unsupported, use the same endpoint's engine mix format.
- Capture sample rate, precision and channels are independent of output settings.
  Temporary WAVs preserve effective precision and channel layout; integer
  padding bytes may be packed out losslessly for decoder compatibility.
- Reads return whole interleaved packets in the actual capture format. The COM
  apartment and interfaces are owned by the recorder thread.
- Start returns only after initialize, open, stream start, and WAV creation.
- Pause stops capture and polls state every ~100 ms; resume resets old buffered
  packets before restarting the stream.
- Ten consecutive read errors terminate recording, with ~10 ms between errors;
  a successful read resets the count.
- Available packets are drained promptly; empty reads wait ~10 ms.
- Stop finalizes the WAV; cancel/lifecycle cancellation removes it.
- Shutdown cancellation is non-blocking.

## Conversion

Both frontends use `dictate_core::embedded_ffmpeg::EmbeddedFfmpegConverter` through
`prepare_audio_for_upload`. Neither searches PATH nor launches FFmpeg.
Release builds enable `dictate-core/static-libav`; builds without native libraries
remain usable for type checks and report LibAvUnavailable on conversion.

The native bridge streams arbitrary supported inputs to 16 kHz mono int16
callbacks for a fresh Earshot 1.2.2 detector per input. Frames contain 256 samples;
the final partial frame is zero-padded, but interval endpoints exclude padding.
Segmentation requires three consecutive frames at or above VAD_START_THRESHOLD
(default 0.6), with up to six candidate frames of lookback including confirmation.
Before activation, a frame below both the start and continuation thresholds clears
the candidate. Continuation uses 0.5; at least four continuation-level frames are
required per segment, and ten silent frames close it. EOF flushes active speech.
The lookback voice count excludes discarded frames. GUI and CLI pass the shared
core configuration to the detector.

Half-open intervals count frames per channel. Starts map down and ends map up
from 16 kHz using u128 arithmetic. Empty intervals are removed; sorted overlapping
intervals merge, clamp to decoded length and receive padding. The first and last
boundaries receive full padding; internal boundaries share one padding, splitting
floor(ms/2) after the preceding segment and the remainder before the next.
Gaps at most one padding remain intact. No fixed interval count limit exists.

The final pass always decodes the original input once, selects intervals with a
forward-only cursor and av_samples_copy, then resamples, queues and encodes.
Packed and planar audio are supported. Output PTS starts at zero and remains
continuous. No libavfilter, filtergraph or intermediate analysis/cropped WAV is
used. Any native failure fails the entire operation and deletes partial output.
Source format changes during decoding fail explicitly.

A blocking worker owns callback state; cancellation is polled during packet,
frame and interval processing and via AVIO interrupt callbacks. Cleanup waits
until native output is closed. Identical input/output paths are rejected.

Input support includes WAV/PCM, MP3, FLAC, Ogg/Opus, Ogg/Vorbis, AAC/M4A/MP4,
ALAC/M4A, WebM/Matroska, WavPack, AC3/EAC3. The build explicitly selects
demuxers, decoders and parsers plus file protocol.

With VAD off, no analysis or trimming occurs. With VAD on and no speech, no ASR
request or text output is created; temporary audio is removed, retry state is
cleared and GUI/hotkey mode returns to Idle. CLI file mode reports the outcome
and exits successfully. KEEP_CACHE otherwise retains original audio, final
audio and successful responses under the existing rules. Both manual retry and
automatic HTTP retry with VAD enabled reanalyze and reconvert the original;
disabled VAD preserves existing HTTP retry behavior.

## Runtime state machine

Visible states are Idle, Recording, Paused, Uploading, Rewriting, and Error. Normal actions
use one action lock. GUI and hotkey events use a non-blocking try-lock and are
dropped when busy; they never queue for a later state. Upload/Rewrite
cancellation bypasses that lock and cancels the active request token directly.

- Idle/Error: start is allowed.
- Idle with a retry buffer: `CANCEL_OR_RETRY_KEY` retries the buffered WAV.
- Recording/Paused: stop and cancel are allowed.
- Pause outside recording is silent except debug output.
- Uploading/Rewriting: cancel is allowed; other inputs are dropped.
- Rewrite holds the action lock from selected-text acquisition through requests, retry waits and shared output. API probes use the same lock and cancellation token; they never select or output text and never update the audio retry buffer.

Stop flow is WAV finalize, Uploading, conversion, ASR, extraction, clipboard
paste, cache handling, then Idle or Error. Empty ASR text goes to Idle without
an `[empty result]` paste. This includes whitespace-only and explicit null
transcripts, completed empty Advanced streams/realtime sessions, and the exact
HTTP 400 `ASR_RESPONSE_HAVE_NO_WORDS` recognition response. Segmented batches omit
empty transcripts without introducing placeholders or separators for them.
A matching stream failure rule carrying `ASR_RESPONSE_HAVE_NO_WORDS` also ends
HTTP recognition immediately with an empty result, without waiting for EOF or
applying trailing events. Ordinary completion still rejects subsequent actions.
Missing required fields, invalid types, incomplete streams, and other request
failures retain their error handling. Eligible terminal audio request failures write a
concise, redacted placeholder after retries are exhausted; Rewrite failures
never write a placeholder. Manual request
cancellation signals the active conversion/upload pipeline, immediately aborts
ASR upload, response, and retry waits, performs cache cleanup, and returns to
Idle without reporting an upload failure.

GUI and CLI hotkey mode retain the latest completed WAV in memory for retry,
up to 100,000,000 bytes. Except for the no-speech result, the buffer survives failed, successful, and manually
canceled requests, is replaced only by another completed recording, and is
released during shutdown. Retry attempts recreate a temporary WAV and do not
create a persistent retry cache.

Shutdown cancels the lifecycle, requests recorder cancellation without waiting,
unregisters hotkeys, and waits at most about 250 ms for the action lock.

## Native GUI

The GUI uses a Win32 message loop, Direct2D/DirectWrite, native controls,
`Shell_NotifyIconW`, and `ITaskbarList.AddTab/DeleteTab`. It contains no
WebView or embedded browser runtime.

- Full window: 222×94 logical pixels.
- Minimal window: 170×46 logical pixels.
- Settings window: 760×662 logical pixels. The prompt editor is up to 700
  logical pixels wide; its height adapts to content, and both dimensions are
  limited to the current monitor work area. Limited height uses a scrolling
  body with a fixed footer; Tab navigation scrolls the focused field into view.
- Frameless, per-pixel-alpha layered, always on top, per-monitor DPI aware.
- Drag threshold is about 4 px; dragging minimal mode from the microphone does
  not activate recording on release.
- Minimal hides settings/status and removes the taskbar tab.
- Full and minimal floating panels always use the same 10 logical pixel Direct2D
  continuous-corner profile, with a fully inset 1 px outline. The layered HWND
  is composed from Direct2D premultiplied-alpha pixels on Windows 10 and Windows
  11, preserving antialiased edges; native DWM rounding and the Windows 11 border
  are disabled so no second outer shape is added.
- Tray menu remains Minimal, Settings, Quit and emits no balloon.
- Audio Record settings expose ENABLE_VAD, VAD_PADDING_MS and VAD_START_THRESHOLD in all five languages.
  Padding and start threshold are disabled while VAD is off, retain their values and are validated on save.
- Settings use native tab/edit/button/checkbox/combobox controls. Token is a
  password edit. Its outer frame uses the same 10 logical pixel continuous-corner
  profile as the floating panels while preserving native child controls. Display
  languages are English, Simplified Chinese, German, Japanese, and French.
- The Display page controls the shared opacity and scale of the full and minimal
  floating windows.
- Saving validates drafts and prepares replacement runtime dependencies/hotkeys under the task lock, atomically replaces `%APPDATA%\dictate\config.json`, then commits runtime state. Failure preserves the previous configuration and restores its hotkeys; rollback registration failures are reported. Saving is allowed only in Idle or Error.
- Escape closes settings first; busy quit shows a native confirmation dialog.

## Verification split

Cross-platform unit tests cover config, JSON path, request fields, cache,
hotkey parsing, clipboard transaction, recorder failure/pause/cancel behavior,
conversion arguments, CLI parsing, and runtime action dropping. The MinGW
build verifies Windows API signatures and static linkage. Real Windows 10/11
manual verification remains required for audio hardware, foreground paste,
low-level hooks, taskbar behavior, DWM corners, tray interaction, and high-DPI
visual comparison.
