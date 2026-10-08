//! Generic wire framing helpers for streamed HTTP recognition responses.
//!
//! These types intentionally stop at the transport boundary: they split SSE
//! and NDJSON into raw events, but do not interpret event names or JSON fields.
//! A workflow-specific layer can therefore map the resulting frames to
//! `TranscriptAction` without coupling this parser to the workflow schema.

use std::error::Error;
use std::fmt;

use serde_json::Value;

/// The maximum size of an individual streamed response unit.
///
/// This limit applies to each SSE physical line, to all retained fields in one
/// SSE event, to each NDJSON line, and to a pending JSON value.  Keeping the
/// same bound at every framing layer prevents a newline or many small SSE
/// fields from bypassing the response-size limit.
const MAX_STREAM_FRAME_BYTES: usize = 1024 * 1024;

/// One raw Server-Sent Events field, preserved in input order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseField {
    /// Field name, without the separating colon.
    pub name: String,
    /// Field value, with at most one leading space after `:` removed.
    pub value: String,
}

/// A complete Server-Sent Events frame.
///
/// Multiple `data` fields are joined with a newline according to SSE framing
/// rules. `fields` retains every non-comment field, including vendor-specific
/// ones and repeated fields, so later workflow code can make its own choices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    /// The most recent `event` field in this frame, if present.
    pub event: Option<String>,
    /// All `data` field values joined by `\n`.
    pub data: String,
    /// The most recent `id` field in this frame, if present.
    pub id: Option<String>,
    /// A syntactically valid `retry` field in milliseconds, if present.
    pub retry: Option<u64>,
    /// Every non-comment field in wire order.
    pub fields: Vec<SseField>,
}

/// A raw non-empty line from an NDJSON response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NdjsonFrame {
    /// One-based physical line number in the stream.
    pub line: usize,
    /// The unparsed JSON text from that line, excluding its line ending.
    pub data: String,
}

/// A framing error before protocol-specific event interpretation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamFrameError {
    /// A complete framed line was not valid UTF-8.
    InvalidUtf8 {
        /// Wire format being decoded.
        format: &'static str,
        /// One-based line number in the input stream.
        line: usize,
        /// Decoder detail from the UTF-8 validator.
        detail: String,
    },
    /// One logical line/event/value exceeded the bounded stream parser
    /// buffer.  A transcription protocol is not an unbounded downloader.
    FrameTooLarge {
        /// Wire format being decoded.
        format: &'static str,
        /// Maximum accepted line, event, or JSON value size.
        maximum: usize,
    },
    /// A JSON-chunks response contained an invalid complete JSON value.
    InvalidJsonChunk {
        /// Parser detail from serde_json.
        detail: String,
    },
}

impl fmt::Display for StreamFrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUtf8 {
                format,
                line,
                detail,
            } => write!(
                formatter,
                "invalid UTF-8 in {format} stream at line {line}: {detail}"
            ),
            Self::FrameTooLarge { format, maximum } => write!(
                formatter,
                "{format} stream contains a line, event, or value larger than {maximum} bytes"
            ),
            Self::InvalidJsonChunk { detail } => {
                write!(formatter, "invalid JSON chunk: {detail}")
            }
        }
    }
}

impl Error for StreamFrameError {}

/// Incrementally decodes complete SSE frames from arbitrary response chunks.
///
/// `push` may receive a chunk in the middle of a UTF-8 code point or a line;
/// it retains incomplete bytes until a following chunk provides the newline.
/// `finish` emits a final unterminated event when it contains fields, which is
/// useful for HTTP servers that close the response without a trailing blank
/// line.
#[derive(Debug)]
pub struct SseDecoder {
    pending_bytes: Vec<u8>,
    pending_event: PendingSseEvent,
    line: usize,
    first_line: bool,
}

impl Default for SseDecoder {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Default)]
struct PendingSseEvent {
    event: Option<String>,
    data_lines: Vec<String>,
    id: Option<String>,
    retry: Option<u64>,
    fields: Vec<SseField>,
    field_bytes: usize,
}

impl PendingSseEvent {
    fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    fn take(&mut self) -> SseEvent {
        self.field_bytes = 0;
        SseEvent {
            event: self.event.take(),
            data: std::mem::take(&mut self.data_lines).join("\n"),
            id: self.id.take(),
            retry: self.retry.take(),
            fields: std::mem::take(&mut self.fields),
        }
    }

    fn add_field_bytes(&mut self, bytes: usize) -> Result<(), StreamFrameError> {
        let Some(total) = self.field_bytes.checked_add(bytes) else {
            return Err(frame_too_large("SSE"));
        };
        if total > MAX_STREAM_FRAME_BYTES {
            return Err(frame_too_large("SSE"));
        }
        self.field_bytes = total;
        Ok(())
    }
}

impl SseDecoder {
    /// Creates an empty SSE decoder.
    pub fn new() -> Self {
        Self {
            pending_bytes: Vec::new(),
            pending_event: PendingSseEvent::default(),
            line: 1,
            first_line: true,
        }
    }

    /// Feeds arbitrary bytes and returns every complete SSE frame they finish.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<SseEvent>, StreamFrameError> {
        self.pending_bytes.extend_from_slice(chunk);

        let mut events = Vec::new();
        let mut consumed = 0;
        while let Some(relative_newline) = self.pending_bytes[consumed..]
            .iter()
            .position(|byte| *byte == b'\n')
        {
            let newline = consumed + relative_newline;
            let mut line = self.pending_bytes[consumed..newline].to_vec();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if let Some(event) = self.process_line(&line)? {
                events.push(event);
            }
            self.line += 1;
            consumed = newline + 1;
        }
        if consumed != 0 {
            self.pending_bytes.drain(..consumed);
        }
        ensure_frame_size("SSE", line_without_trailing_cr(&self.pending_bytes).len())?;

        Ok(events)
    }

    /// Finishes decoding and returns a final unterminated line or frame.
    pub fn finish(mut self) -> Result<Vec<SseEvent>, StreamFrameError> {
        let mut events = Vec::new();
        let line = std::mem::take(&mut self.pending_bytes);
        if !line.is_empty()
            && let Some(event) = self.process_line(&line)?
        {
            events.push(event);
        }
        if !self.pending_event.is_empty() {
            events.push(self.pending_event.take());
        }
        Ok(events)
    }

    fn process_line(&mut self, bytes: &[u8]) -> Result<Option<SseEvent>, StreamFrameError> {
        let bytes = line_without_trailing_cr(bytes);
        ensure_frame_size("SSE", bytes.len())?;
        let mut line = decode_utf8("SSE", self.line, bytes)?;
        if self.first_line {
            self.first_line = false;
            line = line.strip_prefix('\u{feff}').unwrap_or(line);
        }

        if line.is_empty() {
            return Ok((!self.pending_event.is_empty()).then(|| self.pending_event.take()));
        }
        if line.starts_with(':') {
            return Ok(None);
        }

        let (name, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        self.pending_event.add_field_bytes(bytes.len())?;
        self.pending_event.fields.push(SseField {
            name: name.into(),
            value: value.into(),
        });

        match name {
            "event" => self.pending_event.event = Some(value.into()),
            "data" => self.pending_event.data_lines.push(value.into()),
            "id" => self.pending_event.id = Some(value.into()),
            "retry" => self.pending_event.retry = value.parse().ok(),
            _ => {}
        }
        Ok(None)
    }
}

/// Splits a complete in-memory SSE response into raw frames.
pub fn parse_sse_frames(bytes: &[u8]) -> Result<Vec<SseEvent>, StreamFrameError> {
    let mut decoder = SseDecoder::new();
    let mut events = decoder.push(bytes)?;
    events.extend(decoder.finish()?);
    Ok(events)
}

/// Incrementally splits newline-delimited JSON into raw, unparsed lines.
///
/// Empty and whitespace-only lines are ignored. The parser deliberately does
/// not deserialize the JSON: a workflow decides whether each raw JSON value,
/// object, array, or string is meaningful for its protocol.
#[derive(Debug)]
pub struct NdjsonDecoder {
    pending_bytes: Vec<u8>,
    line: usize,
}

impl Default for NdjsonDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl NdjsonDecoder {
    /// Creates an empty NDJSON decoder.
    pub fn new() -> Self {
        Self {
            pending_bytes: Vec::new(),
            line: 1,
        }
    }

    /// Feeds arbitrary bytes and returns every complete non-empty NDJSON line.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<NdjsonFrame>, StreamFrameError> {
        self.pending_bytes.extend_from_slice(chunk);

        let mut frames = Vec::new();
        let mut consumed = 0;
        while let Some(relative_newline) = self.pending_bytes[consumed..]
            .iter()
            .position(|byte| *byte == b'\n')
        {
            let newline = consumed + relative_newline;
            let mut line = &self.pending_bytes[consumed..newline];
            if line.last() == Some(&b'\r') {
                line = &line[..line.len() - 1];
            }
            if let Some(frame) = self.frame(line)? {
                frames.push(frame);
            }
            self.line += 1;
            consumed = newline + 1;
        }
        if consumed != 0 {
            self.pending_bytes.drain(..consumed);
        }
        ensure_frame_size(
            "NDJSON",
            line_without_trailing_cr(&self.pending_bytes).len(),
        )?;

        Ok(frames)
    }

    /// Finishes decoding and accepts a final non-empty line without `\n`.
    pub fn finish(self) -> Result<Vec<NdjsonFrame>, StreamFrameError> {
        if self.pending_bytes.is_empty() {
            return Ok(Vec::new());
        }
        let mut line = self.pending_bytes.as_slice();
        if line.last() == Some(&b'\r') {
            line = &line[..line.len() - 1];
        }
        Ok(self.frame(line)?.into_iter().collect())
    }

    fn frame(&self, bytes: &[u8]) -> Result<Option<NdjsonFrame>, StreamFrameError> {
        ensure_frame_size("NDJSON", bytes.len())?;
        let data = decode_utf8("NDJSON", self.line, bytes)?;
        if data.trim().is_empty() {
            Ok(None)
        } else {
            Ok(Some(NdjsonFrame {
                line: self.line,
                data: data.into(),
            }))
        }
    }
}

/// A stream of consecutive JSON values, as used by APIs that use chunked
/// transfer without newline delimiters.  Unlike NDJSON, boundaries are found
/// through the JSON parser and may split across arbitrary network chunks.
#[derive(Debug, Default)]
pub struct JsonChunksDecoder {
    pending_bytes: Vec<u8>,
}

impl JsonChunksDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns fully parsed JSON values. An incomplete trailing value stays
    /// buffered until a later call or [`Self::finish`].
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<String>, StreamFrameError> {
        self.pending_bytes.extend_from_slice(chunk);
        self.decode(false)
    }

    /// Returns any final complete values and rejects an incomplete trailing
    /// JSON value at end-of-stream.
    pub fn finish(mut self) -> Result<Vec<String>, StreamFrameError> {
        self.decode(true)
    }

    fn decode(&mut self, final_chunk: bool) -> Result<Vec<String>, StreamFrameError> {
        let leading = self
            .pending_bytes
            .iter()
            .take_while(|byte| byte.is_ascii_whitespace())
            .count();
        if leading != 0 {
            self.pending_bytes.drain(..leading);
        }
        if self.pending_bytes.is_empty() {
            return Ok(Vec::new());
        }
        ensure_frame_size("JSON chunks", self.pending_bytes.len())?;

        let mut decoder =
            serde_json::Deserializer::from_slice(&self.pending_bytes).into_iter::<Value>();
        let mut values = Vec::new();
        let mut consumed = 0;
        while let Some(item) = decoder.next() {
            match item {
                Ok(value) => {
                    consumed = decoder.byte_offset();
                    values.push(value.to_string());
                }
                Err(error) if !final_chunk && error.is_eof() => break,
                Err(error) => {
                    return Err(StreamFrameError::InvalidJsonChunk {
                        detail: error.to_string(),
                    });
                }
            }
        }
        if consumed != 0 {
            self.pending_bytes.drain(..consumed);
        }
        if final_chunk {
            if !self.pending_bytes.iter().all(u8::is_ascii_whitespace) {
                return Err(StreamFrameError::InvalidJsonChunk {
                    detail: "stream ended with an incomplete JSON value".into(),
                });
            }
            self.pending_bytes.clear();
        }
        Ok(values)
    }
}

/// Splits a complete in-memory NDJSON response into raw frames.
pub fn parse_ndjson_frames(bytes: &[u8]) -> Result<Vec<NdjsonFrame>, StreamFrameError> {
    let mut decoder = NdjsonDecoder::new();
    let mut frames = decoder.push(bytes)?;
    frames.extend(decoder.finish()?);
    Ok(frames)
}

fn decode_utf8<'a>(
    format: &'static str,
    line: usize,
    bytes: &'a [u8],
) -> Result<&'a str, StreamFrameError> {
    std::str::from_utf8(bytes).map_err(|error| StreamFrameError::InvalidUtf8 {
        format,
        line,
        detail: error.to_string(),
    })
}

fn line_without_trailing_cr(bytes: &[u8]) -> &[u8] {
    if bytes.last() == Some(&b'\r') {
        &bytes[..bytes.len() - 1]
    } else {
        bytes
    }
}

fn ensure_frame_size(format: &'static str, size: usize) -> Result<(), StreamFrameError> {
    if size > MAX_STREAM_FRAME_BYTES {
        return Err(frame_too_large(format));
    }
    Ok(())
}

fn frame_too_large(format: &'static str) -> StreamFrameError {
    StreamFrameError::FrameTooLarge {
        format,
        maximum: MAX_STREAM_FRAME_BYTES,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_parses_crlf_data_lines_known_fields_and_vendor_fields() {
        let events = parse_sse_frames(
            b": keepalive\r\nevent: transcript\r\nid: 42\r\nretry: 1500\r\ndata: {\"delta\":\"hel\"}\r\ndata: lo\r\nx-vendor-kind: token\r\n\r\n",
        )
        .unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event.as_deref(), Some("transcript"));
        assert_eq!(events[0].id.as_deref(), Some("42"));
        assert_eq!(events[0].retry, Some(1500));
        assert_eq!(events[0].data, "{\"delta\":\"hel\"}\nlo");
        assert_eq!(
            events[0].fields,
            vec![
                SseField {
                    name: "event".into(),
                    value: "transcript".into(),
                },
                SseField {
                    name: "id".into(),
                    value: "42".into(),
                },
                SseField {
                    name: "retry".into(),
                    value: "1500".into(),
                },
                SseField {
                    name: "data".into(),
                    value: "{\"delta\":\"hel\"}".into(),
                },
                SseField {
                    name: "data".into(),
                    value: "lo".into(),
                },
                SseField {
                    name: "x-vendor-kind".into(),
                    value: "token".into(),
                },
            ]
        );
    }

    #[test]
    fn sse_handles_chunk_boundaries_utf8_bom_and_eof_frame() {
        let mut decoder = SseDecoder::new();

        assert!(
            decoder
                .push(b"\xEF\xBB\xBFevent: final\ndata: caf\xC3")
                .unwrap()
                .is_empty()
        );
        assert!(decoder.push(b"\xA9").unwrap().is_empty());
        let events = decoder.finish().unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event.as_deref(), Some("final"));
        assert_eq!(events[0].data, "café");
    }

    #[test]
    fn sse_preserves_a_named_no_data_frame_for_protocol_specific_completion() {
        let events = parse_sse_frames(b"event: complete\n\n").unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event.as_deref(), Some("complete"));
        assert_eq!(events[0].data, "");
    }

    #[test]
    fn sse_reports_invalid_utf8_with_format_and_line() {
        let error = parse_sse_frames(b"data: \xFF\n\n").unwrap_err();

        assert!(matches!(
            error,
            StreamFrameError::InvalidUtf8 {
                format: "SSE",
                line: 1,
                ..
            }
        ));
        assert!(
            error
                .to_string()
                .contains("invalid UTF-8 in SSE stream at line 1")
        );
    }

    #[test]
    fn sse_rejects_an_oversized_complete_line() {
        let oversized_line = format!(":{}\n", "x".repeat(MAX_STREAM_FRAME_BYTES));
        let mut decoder = SseDecoder::new();

        assert_eq!(
            decoder.push(oversized_line.as_bytes()),
            Err(StreamFrameError::FrameTooLarge {
                format: "SSE",
                maximum: MAX_STREAM_FRAME_BYTES,
            })
        );
    }

    #[test]
    fn sse_rejects_many_small_data_lines_that_exceed_one_event_limit() {
        let data_line = format!("data: {}\n", "x".repeat(MAX_STREAM_FRAME_BYTES / 2));
        let mut decoder = SseDecoder::new();

        assert!(decoder.push(data_line.as_bytes()).unwrap().is_empty());
        assert_eq!(
            decoder.push(data_line.as_bytes()),
            Err(StreamFrameError::FrameTooLarge {
                format: "SSE",
                maximum: MAX_STREAM_FRAME_BYTES,
            })
        );
    }

    #[test]
    fn default_decoders_have_the_same_initial_state_as_new() {
        let sse_events = SseDecoder::default().finish().unwrap();
        assert!(sse_events.is_empty());

        let ndjson_frames = NdjsonDecoder::default().finish().unwrap();
        assert!(ndjson_frames.is_empty());

        let mut ndjson = NdjsonDecoder::default();
        let error = ndjson.push(b"\xFF\n").unwrap_err();
        assert!(matches!(
            error,
            StreamFrameError::InvalidUtf8 {
                format: "NDJSON",
                line: 1,
                ..
            }
        ));
    }

    #[test]
    fn ndjson_handles_chunks_crlf_blank_lines_and_final_line_without_newline() {
        let mut decoder = NdjsonDecoder::new();

        assert_eq!(
            decoder.push(b"{\"delta\":\"he").unwrap(),
            Vec::<NdjsonFrame>::new()
        );
        let first = decoder.push(b"llo\"}\r\n \t\r\n").unwrap();
        assert_eq!(
            first,
            vec![NdjsonFrame {
                line: 1,
                data: "{\"delta\":\"hello\"}".into(),
            }]
        );
        assert!(decoder.push(b"{\"done\":true}").unwrap().is_empty());
        assert_eq!(
            decoder.finish().unwrap(),
            vec![NdjsonFrame {
                line: 3,
                data: "{\"done\":true}".into(),
            }]
        );
    }

    #[test]
    fn ndjson_is_raw_and_does_not_assume_a_json_shape() {
        let frames = parse_ndjson_frames(b"[1,2]\n\"a string\"\ntrue\n").unwrap();

        assert_eq!(
            frames,
            vec![
                NdjsonFrame {
                    line: 1,
                    data: "[1,2]".into(),
                },
                NdjsonFrame {
                    line: 2,
                    data: "\"a string\"".into(),
                },
                NdjsonFrame {
                    line: 3,
                    data: "true".into(),
                },
            ]
        );
    }

    #[test]
    fn ndjson_reports_invalid_utf8_on_the_correct_line() {
        let error = parse_ndjson_frames(b"{\"ok\":true}\n\xFF\n").unwrap_err();

        assert!(matches!(
            error,
            StreamFrameError::InvalidUtf8 {
                format: "NDJSON",
                line: 2,
                ..
            }
        ));
        assert!(
            error
                .to_string()
                .contains("invalid UTF-8 in NDJSON stream at line 2")
        );
    }

    #[test]
    fn ndjson_rejects_an_oversized_complete_line() {
        let oversized_line = format!("{}\n", "x".repeat(MAX_STREAM_FRAME_BYTES + 1));

        assert_eq!(
            parse_ndjson_frames(oversized_line.as_bytes()),
            Err(StreamFrameError::FrameTooLarge {
                format: "NDJSON",
                maximum: MAX_STREAM_FRAME_BYTES,
            })
        );
    }

    #[test]
    fn json_chunks_retains_a_partial_value_and_accepts_consecutive_values() {
        let mut decoder = JsonChunksDecoder::new();
        assert!(decoder.push(br#"{"delta":"he"#).unwrap().is_empty());
        assert_eq!(
            decoder.push(br#"llo"}{"done":true}"#).unwrap(),
            [r#"{"delta":"hello"}"#, r#"{"done":true}"#]
        );
        assert!(decoder.finish().unwrap().is_empty());
    }

    #[test]
    fn json_chunks_rejects_an_incomplete_final_value() {
        let mut decoder = JsonChunksDecoder::new();
        assert!(decoder.push(br#"{"delta":"hello"#).unwrap().is_empty());
        assert!(matches!(
            decoder.finish(),
            Err(StreamFrameError::InvalidJsonChunk { .. })
        ));
    }
}
