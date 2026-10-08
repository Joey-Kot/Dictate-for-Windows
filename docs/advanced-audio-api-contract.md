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

The current workflow schema version is `1`. A workflow must include
`schema_version`, `name`, `audio`, and `recognition`; `parameters` and
`secrets` may be omitted and then default to empty arrays. When Advanced is
enabled, or when a workflow is explicitly validated, an unknown schema version
fails explicitly. It is never guessed or silently executed. A disabled draft is
inert so a user can retain it while using Legacy.

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
IDs, labels, required state, and optional value defaults. Defaults are literal
strings, not a second template-evaluation phase. A workflow must never embed a
real credential.

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

## Template security boundary

The only supported template values are:

```text
{{var:id}}              {{secret:id}}           {{capture:id}}
{{audio:filename}}      {{audio:mime}}          {{audio:size}}
{{audio:base64}}        {{audio:data_uri}}      {{audio:public_url}}
{{audio:cloud_uri}}     {{audio:chunk_base64}}
{{runtime:uuid}}        {{runtime:unix_seconds}} {{runtime:unix_millis}}
```

There are no template functions, expressions, conditionals, loops, scripts,
environment variables, file references, shells, registry access, dynamic DLLs,
or arbitrary disk writes. The validator verifies declared variables/secrets,
capture ordering, fixed namespaces, and compatible audio placeholders before
any network connection is made. The workflow cannot select an arbitrary local
path: audio comes only from the current prepared recording, recorder stream,
retry recording, or an audio file explicitly supplied to CLI `--file`.

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
workflow. Version 1 supports WebDAV, S3-Compatible storage, and Aliyun OSS.
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

## Realtime version-1 behavior

Only WebSocket realtime is implemented. It accepts `pcm_s16le`, realtime
pacing, 1–8 channels, sample rates from 1 to 384000 Hz, and chunk durations
from 10 to 1000 ms. `unbounded` pacing and `keep_session` pause behavior are
rejected; pause finalizes the current session and resume starts a new one.

Dictate retains the complete local WAV while live audio is sent. On live
network failure, or cancellation during live finalization, it discards all live
partial and committed transcript state, completes the local recording, and
replays the audio from zero. Each replay retry creates a fresh session from
audio zero, never merging partial text from an earlier attempt.

## Privacy, diagnostics, and limits

With `UPLOAD_DEBUG`, Advanced operations log only fixed phase labels. They do
not log rendered URLs, query strings, headers, bodies, audio, captures, or
transcripts. Workflow secrets, remote-storage credentials, URL userinfo/query
credentials, presigned URLs, AWS/TC3 signatures, and values from sensitive
captures are redacted before diagnostic or error-response paths. Debug output
for Advanced config and remote references also redacts credentials and cleanup
details. Before sending vendor material to Rewrite for GUI workflow generation,
the GUI applies best-effort credential redaction.

The bounded limits are: workflow JSON 256 KiB; one template 32 KiB; at most
64 parameters, secrets, headers, query entries, captures, or stream rules; at
most 32 initial and 32 finish messages; poll interval at least 100 ms and poll
timeout at most 24 hours. Base64/Data-URI source audio is limited to 16 MiB.
HTTP responses, streamed raw responses, and WebSocket messages are limited to
32 MiB; body-based extraction is limited to 2 MiB; each SSE event, NDJSON line,
or JSON chunk is limited to 1 MiB.

HTTP workflows may use `http` or `https`, and WebSocket workflows may use
`ws` or `wss`; localhost and LAN targets are allowed. A
`public_https_url` remains HTTPS-only.

## Deliberately unsupported in version 1

- gRPC and custom HTTP/2 event streams;
- callback-only or webhook-only completion;
- arbitrary workflows, branches, loops, or user scripts;
- custom signer code and provider-specific realtime resume;
- live partial insertion into the foreground application.
