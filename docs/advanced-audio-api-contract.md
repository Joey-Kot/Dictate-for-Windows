# Advanced Audio API contract

## Purpose

Advanced Audio API is a finite, declarative description of an ASR network
protocol. It is not a provider registry or a programmable workflow engine.

The feature is disabled by default. When `ADVANCED_AUDIO_API.enabled` is
`false` or the key is absent, Dictate uses the existing Legacy Audio API and
keeps all existing `API_ENDPOINT`, `TOKEN`, `MODEL`, `LANGUAGE`, `PROMPT`,
`TEXT_PATH`, and `ExtraConfig` behavior unchanged.

## Ownership

- `dictate-gui` generates, edits, validates, tests, and stores workflows.
- `dictate-core` owns the serializable schema, validator, templates,
  transports, authentication, remote audio, cancellation, and execution.
- `dictate-cli` only loads config and uses the same Core client as GUI/hotkey
  mode. It has no workflow-generator dependency.

## Versioning and configuration

The GUI compiler prompt requests schema version `2` for newly generated
workflows. This build accepts both versions `1` and `2`. A workflow must include
`schema_version`, `name`, `audio`, and `recognition`; `parameters` and
`secrets` may be omitted and then default to empty arrays. When Advanced is
enabled, or when a workflow is explicitly validated, an unknown schema version
fails explicitly. It is never guessed or silently executed. A disabled draft is
inert so a user can retain it while using Legacy.

Version 1 remains supported for existing saved workflows without automatic
upgrade. Its parameter declarations are text-only and must not contain the
version-2 `type`, `options`, or `visible_when` fields. Version 2 requires a
`type` for every parameter and adds the finite typed-input model described
below.

`ADVANCED_AUDIO_API` has this stored shape:

```json
{
  "enabled": false,
  "workflow": null,
  "values": {},
  "secrets": {},
  "remote_audio": { "type": "none" }
}
```

Values and secrets are separate from the workflow. A workflow declares their
IDs, labels, required state, and optional value defaults. `values` remains a
map of strings even for version-2 parameters, preserving the stored
configuration shape. Defaults are likewise literal strings, not a second
template-evaluation phase. A workflow must never embed a real credential.

For example, a version-2 configuration can store an integer, a boolean, a
multi-select value, a JSON object, and a JSON array as follows:

```json
{
  "sample_rate": "16000",
  "timestamps": "true",
  "languages": "[\"zh\",\"en\"]",
  "vocabulary": "{\"wake_phrase\":\"Dictate\"}",
  "language_hints": "[\"zh\",\"en\"]"
}
```

## Dynamic parameters and rendering

Version 2 supports the parameter types `text`, `integer`, `number`,
`boolean`, `select`, `multi_select`, `json_object`, and `json_array`. The GUI
presents text, integer, and number values as text inputs; boolean as a
checkbox; `select` as a single choice; `multi_select` as a multiple-choice
list; and `json_object` or `json_array` as a scrollable multiline JSON editor.
The two JSON editors each occupy a dedicated dynamic-form page. `select` and
`multi_select` require a nonempty set of distinct `options`, each with a stable
string `value` and a display `label`; options are not valid for JSON-object or
JSON-array parameters.

The stored string must be valid for its declared type: `integer` and `number`
use JSON number text, `boolean` is exactly `true` or `false`, and `select`
uses one declared option value. A `multi_select` value or default is a string
whose contents are a JSON array of distinct declared option values, such as
`"[\"zh\",\"en\"]"`. A `json_object` value or default is a string whose
contents parse as a JSON object; a `json_array` value or default is a string
whose contents parse as a JSON array. None of these is stored as a raw JSON
value in configuration.

Typed conversion is deliberately narrow. In a version-2 JSON body or JSON
realtime message, only a JSON string leaf that is exactly `{{var:id}}` renders
as that parameter's native JSON value: integers and numbers become JSON
numbers, booleans become JSON booleans, `multi_select` and `json_array` become
JSON arrays, and `json_object` becomes a JSON object. `text` and `select`
remain JSON strings. A variable embedded in a larger JSON string renders as
text only for scalar types. Version-1 variables remain text even when they
occupy a complete JSON leaf.

`multi_select` has no implicit CSV or other text serialization; `json_object`
and `json_array` have no text serialization at all. All three are rejected
before network I/O if used in a JSON string other than a complete leaf or in
any string template context, including HTTP URL, query, and header values,
URL-encoded forms, multipart text or bytes, raw bytes, WebSocket connection
URL/query/header/subprotocol fields, and realtime text or binary messages.
This avoids silently changing the provider request shape.

`visible_when` is version-2 GUI presentation metadata only. It contains a
source parameter plus exactly one of `equals` or a nonempty `one_of` array. The
source must be an earlier, unconditional boolean or `select` parameter with a
default, and comparison values must belong to that source's domain. A visibility
condition creates neither a workflow branch nor conditional request fields.
When a control becomes hidden, its stored value remains present and normal
required-value validation still applies.

## Bounded protocol model

Supported recognition modes are:

- `request`: one HTTP request and final response.
- `request_stream`: one HTTP request followed by SSE, NDJSON, or JSON chunks.
- `async_poll`: optional prepare, exactly one submit, optional repeated poll,
  and at most two result requests.
- `realtime_session`: a WebSocket session using live chunks or replayed audio.

HTTP stages support `GET`, `POST`, `PUT`, `PATCH`, and `DELETE`; URL, query,
headers, typed request bodies, accepted statuses, built-in signer selection,
and response captures. Body types are `none`, `json`, `form_urlencoded`,
`multipart`, `raw_audio`, and `raw_bytes`. Multipart audio is a typed
`audio_file` part rather than a text path. Extractors are JSONPath, response
header, plain body, and HTTP status. Captures can only be used by later stages;
`Capture.sensitive` marks a captured value for redaction. A capture used as an
entire dynamic HTTP-stage URL is also treated as sensitive automatically, so a
temporary URL with query credentials cannot be retained in later error bodies
or cached raw responses.

An HTTP-stage URL normally has a literal `http://` or `https://` prefix. When
an earlier documented response provides a complete absolute HTTP(S) URL, a
later HTTP stage may use that capture as its entire URL, for example
`{{capture:result_url}}`. Its rendered value is checked for an absolute
HTTP(S) URL with a host before a request is created. Relative paths,
non-HTTP(S) schemes, and other template-only stage URLs are rejected.

Audio delivery is explicitly one of `multipart_file`, `raw_audio`, `base64`,
`data_uri`, `public_https_url`, `cloud_uri`, `provider_upload`, or
`realtime_chunks`. HTTPS URLs and cloud URIs such as `s3://` are distinct and
are never implicitly converted. `provider_upload` is only an `async_poll`
shape: its prepare stage uploads the local audio and captures the provider
reference used by submit. A streaming response request can capture only its
headers or status, because its body is not materialized before stream handling.

Only poll may repeat. There are no arbitrary stage arrays, branches, loops,
callbacks, webhooks, or executable workflow code.

`async_poll` permits only GET or POST polling, requires pending, success, and
failure conditions, and never resends audio or an audio reference in poll.
Submit is not automatically retried because a service may already have created
a task. Poll retries remain in poll and read-only result steps may retry; none
of them re-enter submit.

Built-in signer choices are `none`, `aws_sigv4`, and `tencent_tc3`. Signing
algorithms are Core code, never workflow-provided code. Dynamic signers cannot
be used with streaming multipart or `raw_audio` uploads because those bodies
are intentionally not materialized merely to calculate a payload hash.

## Segmented-upload runtime behavior

Segmented upload is an opt-in application runtime policy. It is not an
`ADVANCED_AUDIO_API` workflow field, an `audio.delivery` value, or a
provider-specific extension. Its settings live in the top-level Network
configuration:

```json
{
  "ENABLE_SEGMENTED_UPLOAD": false,
  "MAX_UPLOAD_SEGMENT_SECONDS": 300,
  "MIN_UPLOAD_PAUSE_MS": 700,
  "MAX_UPLOAD_CONCURRENCY": 1
}
```

The switch defaults to `false`. The three numeric values respectively express
the strict maximum source-timeline span of one segment, the minimum qualifying
pause duration, and the maximum number of complete segment workflows that may
be active at once. Each numeric value must be greater than zero even while the
switch is off, so a disabled configuration remains valid only when its retained
settings are valid too.

When enabled, the policy applies to Legacy recognition and to every non-realtime
Advanced execution: `request`, `request_stream`, and `async_poll`, including
their ordinary file delivery, `provider_upload`, `public_https_url`, and
`cloud_uri` paths. `realtime_session` is explicitly excluded even if the switch
is on; it keeps its existing live/replay session behavior and is never sent to
the segmented batch path.

For a newly built plan, libav decodes the original media in its source-frame
domain and performs amplitude-based silence analysis to find pauses that meet
`MIN_UPLOAD_PAUSE_MS`. This is not the Earshot VAD trimming path used by normal
`ENABLE_VAD` preparation. During segmented export, that trimming path is
disabled: pauses only choose boundaries and are never removed, padded away, or
concatenated out of the audio.

The resulting `SegmentPlan` is a contiguous, non-overlapping partition of the
complete original source timeline using half-open frame ranges. Before a strict
maximum boundary, planning prefers the end of the latest eligible pause. If a
pause crosses that boundary, or no eligible pause exists, the boundary is a
hard cut at the configured maximum. Each range is exported as its own
standalone media file. Thus every segment, including internal silence, remains
on the original timeline; the batch neither drops nor duplicates source-frame
ranges. If analysis finds that the entire source is silent, preparation returns
`No speech detected` and sends no segment.

Each exported file runs one complete existing `AudioApiClient` workflow. The
concurrency setting limits complete workflows, not merely upload HTTP requests:
for Advanced remote or asynchronous paths it includes publishing, submit,
polling, result retrieval, and the workflow's cleanup. On the first segment
failure or user cancellation, no additional segment is scheduled; already
started workflows receive cancellation and are allowed to finish their normal
cleanup before the batch returns. A batch produces text only when every segment
succeeds, then concatenates the segment texts in original source order without
inserting spaces, punctuation, or another separator. Failure or cancellation
does not produce a partial transcript.

For an interactive retryable recording, Dictate retains the original WAV under
the existing in-memory retry limit and freezes its upload mode, `SegmentPlan`,
maximum duration, minimum-pause value, and concurrency. A newly selected plan
is retained before export begins, so even an export failure does not require a
new analysis on Retry. Retry re-exports every segment and reruns the complete
batch; it does not re-analyze pauses or reuse any previously successful segment
text. The Settings Audio API Test workflow and CLI `--file` use the same
segmented preparation and bounded-batch path when it applies. The `--file`
input remains caller-owned and is never moved or deleted.

## Template security boundary

The only supported template values are:

```text
{{var:id}}              {{secret:id}}           {{capture:id}}
{{audio:filename}}      {{audio:mime}}          {{audio:size}}
{{audio:base64}}        {{audio:data_uri}}      {{audio:public_url}}
{{audio:cloud_uri}}     {{audio:chunk_base64}}
{{runtime:uuid}}        {{runtime:unix_seconds}} {{runtime:unix_millis}}
```

There are no template functions, expressions, request-time conditionals, loops,
scripts, environment variables, file references, shells, registry access,
dynamic DLLs, or arbitrary disk writes. The validator verifies declared
variables/secrets,
capture ordering, fixed namespaces, and compatible audio placeholders before
any network connection is made. The workflow cannot select an arbitrary local
path: audio comes only from the current prepared audio file (the current
independently exported segment in segmented mode), recorder stream, retry
recording, or an audio file explicitly supplied to CLI `--file`.

`audio:base64`, `audio:data_uri`, `audio:public_url`, and `audio:cloud_uri`
are each restricted to their matching audio-delivery type. Realtime reserves
`audio:chunk_base64` for the per-chunk audio message. Connection URL, query,
headers, subprotocol, initial messages, and finish messages cannot use any
`audio:*` placeholder; realtime text and JSON audio messages must actually use
`{{audio:chunk_base64}}`.

## Transcript and cancellation semantics

Streaming protocols normalize events to `ignore`, `append_delta`,
`replace_partial`, `commit_segment`, `set_final_text`, `complete`, and `fail`.
An authoritative final text wins. A stream that ends without `complete` never
produces an incomplete transcript. Stream receive rules must have an explicit
`complete` action. A realtime workflow may instead use its independently
declared `completion` event or JSONPath condition; text-bearing actions require
a JSONPath, and an `equals` match requires a path.

Cancellation covers upload, HTTP request/response, poll waits, result fetch,
WebSocket operations, replay pacing, finalization, and best-effort remote
cleanup. All modes return one final transcript; partial text is never inserted
into the foreground application.

## Remote audio hosting

Remote hosting is configured in Advanced settings rather than embedded in a
workflow. The current implementation supports WebDAV, S3-Compatible storage,
and Aliyun OSS.
WebDAV keeps its upload endpoint separate from the public HTTPS download URL,
and can provide only `public_https_url`. S3-Compatible and Aliyun OSS can
provide a public HTTPS URL or their respective cloud URI; neither representation
is converted into the other.

For a successfully published object, cleanup occurs after the whole
recognition workflow, including poll and result stages. The
`delete_after_recognition` setting controls normal cleanup after a successful
recognition; cleanup failure never replaces a transcript or cancellation
outcome. Once an upload request has been issued, an upload failure, failed
recognition, or cancellation forces a best-effort DELETE regardless of that
setting, while retaining the original error.

In a segmented non-realtime batch, every started segment has its own remote
publication, recognition, and cleanup lifecycle. A batch never turns several
segments into one shared remote object. If a batch fails or is canceled, it
stops launching later segments, cancels the started workflows, and waits for
their existing per-object cleanup paths before returning.

## Realtime behavior

Only WebSocket realtime is implemented. It accepts `pcm_s16le`, realtime
pacing, 1–8 channels, sample rates from 1 to 384000 Hz, and chunk durations
from 10 to 1000 ms. `unbounded` pacing and `keep_session` pause behavior are
rejected; pause finalizes the current session and resume starts a new one.

Dictate retains the complete local WAV while live audio is sent. On live
network failure, or cancellation during live finalization, it discards all live
partial and committed transcript state, completes the local recording, and
replays the audio from zero. Each replay retry creates a fresh session from
audio zero, never merging partial text from an earlier attempt.

## GUI workflow-generation input

The GUI keeps the user's intended outcome or preferences separate from vendor
documentation and request/response examples. Before calling the configured
Rewrite API, it applies best-effort credential redaction to both inputs and
serializes them as application-generated data:

```json
{
  "input_version": 1,
  "user_requirements": "...",
  "vendor_material": "..."
}
```

The application assigns each role from the envelope fields; labels, delimiters,
or role claims inside either string do not change that classification. The
compiler prompt directs it to treat both fields as data rather than
instructions: `user_requirements` may influence only choices documented by
`vendor_material` and supported by the schema, while `vendor_material` is used
only as evidence for protocol facts. If vendor material does not establish a
requested field's request location and required wire shape, the prompt directs
the compiler to return `needs_more_information`; it directs `unsupported` only
when vendor material explicitly establishes a necessary protocol requirement or
JSON shape that the supplied schema cannot express. The prompt does not
authorize either field to override the schema, safety rules, or required JSON
output shape. This input classification and prompt constraint is not a claim
that an LLM is immune to every prompt-injection attempt.

The GUI does not impose a manual character limit on either input. The native
multiline controls use their maximum supported text limit, so a request that is
too large is left for the configured Rewrite API to reject according to its own
request or token limits.

## Privacy, diagnostics, and limits

With `UPLOAD_DEBUG`, Advanced operations log only fixed phase labels. They do
not log rendered URLs, query strings, headers, bodies, audio, captures, or
transcripts. Workflow secrets, remote-storage credentials, URL userinfo/query
credentials, presigned URLs, AWS/TC3 signatures, and values from sensitive
captures are redacted before diagnostic or error-response paths. Debug output
for Advanced config and remote references also redacts credentials and cleanup
details. Before sending GUI workflow-generation input to Rewrite, the GUI
applies best-effort credential redaction to both user requirements and vendor
material.

The bounded limits are: workflow JSON 256 KiB; one template 32 KiB; at most
64 parameters, secrets, headers, query entries, captures, or stream rules; at
most 32 initial and 32 finish messages; poll interval at least 100 ms and poll
timeout at most 24 hours. Base64/Data-URI input is limited to 16 MiB for each
current prepared audio file. In segmented mode, that is a limit for each
exported segment rather than an aggregate limit on the original recording.
HTTP responses, streamed raw responses, and WebSocket messages are limited to
32 MiB; body-based extraction is limited to 2 MiB; each SSE event, NDJSON line,
or JSON chunk is limited to 1 MiB.

HTTP workflows may use `http` or `https`, and WebSocket workflows may use
`ws` or `wss`; localhost and LAN targets are allowed. A
`public_https_url` remains HTTPS-only.

## Deliberately unsupported

- gRPC and custom HTTP/2 event streams;
- callback-only or webhook-only completion;
- arbitrary workflows, branches, loops, or user scripts;
- custom signer code and provider-specific realtime resume;
- live partial insertion into the foreground application.
