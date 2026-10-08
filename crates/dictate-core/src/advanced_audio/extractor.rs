//! Safe, schema-driven extraction from completed HTTP responses.
//!
//! This module deliberately returns only scalar strings.  It does not retain a
//! response body, and none of its error messages include response or header
//! values, because either can contain provider credentials.

use std::collections::BTreeMap;

use reqwest::header::{HeaderMap, HeaderName};
use thiserror::Error;

use super::schema::{Capture, ResponseExtractor};

/// Largest response body accepted by a body-based extractor.
pub const MAX_RESPONSE_BODY_BYTES: usize = 2 * 1024 * 1024;
/// Largest individual extracted string, including a capture value.
pub const MAX_EXTRACTED_VALUE_BYTES: usize = 256 * 1024;
/// Runtime defence in depth for workflows that bypass schema validation.
pub const MAX_CAPTURE_COUNT: usize = 64;
/// Largest combined size of all values captured from one response.
pub const MAX_CAPTURE_BYTES: usize = 512 * 1024;

/// Borrowed response parts required by [`extract`] and [`extract_captures`].
///
/// Values are intentionally private so callers do not accidentally derive or
/// log a representation containing the response body.
#[derive(Clone, Copy)]
pub struct ResponseData<'a> {
    status: u16,
    headers: &'a HeaderMap,
    body: &'a [u8],
}

impl<'a> ResponseData<'a> {
    /// Creates a response view from the HTTP status, headers, and raw body.
    pub const fn new(status: u16, headers: &'a HeaderMap, body: &'a [u8]) -> Self {
        Self {
            status,
            headers,
            body,
        }
    }

    /// Returns the HTTP status code supplied by the transport.
    pub const fn status(&self) -> u16 {
        self.status
    }
}

/// Errors from a response extractor.
///
/// No variant stores response-body or header-value text.  This lets callers
/// surface the errors in UI and debug output without exposing a response
/// secret.
#[derive(Debug, Error)]
pub enum ResponseExtractionError {
    #[error("response body exceeds the {MAX_RESPONSE_BODY_BYTES}-byte extraction limit")]
    ResponseTooLarge,
    #[error("extracted response value exceeds the {MAX_EXTRACTED_VALUE_BYTES}-byte limit")]
    ValueTooLarge,
    #[error("response body is not valid UTF-8")]
    InvalidBodyUtf8,
    #[error("response body is not valid JSON (line {line}, column {column})")]
    InvalidJson { line: usize, column: usize },
    #[error("invalid JSONPath response extractor")]
    InvalidJsonPath,
    #[error("JSONPath matched no values; expected exactly one")]
    JsonPathNoMatch,
    #[error("JSONPath matched {count} values; expected exactly one")]
    JsonPathMultipleMatches { count: usize },
    #[error("JSONPath selected {kind}; expected a string, number or boolean")]
    JsonPathInvalidType { kind: &'static str },
    #[error("response extractor contains an invalid HTTP header name")]
    InvalidHeaderName,
    #[error("response header '{name}' was not present")]
    HeaderMissing { name: String },
    #[error("response header '{name}' is not valid UTF-8")]
    HeaderNotUtf8 { name: String },
    #[error("response header '{name}' exceeds the {MAX_EXTRACTED_VALUE_BYTES}-byte limit")]
    HeaderTooLarge { name: String },
    #[error("workflow contains more than {MAX_CAPTURE_COUNT} captures")]
    TooManyCaptures,
    #[error("capture '{id}' is declared more than once")]
    DuplicateCapture { id: String },
    #[error("combined captured response data exceeds the {MAX_CAPTURE_BYTES}-byte limit")]
    CapturesTooLarge,
    #[error("failed to extract capture '{id}': {source}")]
    Capture {
        id: String,
        #[source]
        source: Box<ResponseExtractionError>,
    },
}

/// Extracts one scalar value using a workflow [`ResponseExtractor`].
///
/// JSONPath semantics, including scalar conversion and the exactly-one-match
/// rule, are shared with the legacy `TEXT_PATH` implementation.
pub fn extract(
    response: &ResponseData<'_>,
    extractor: &ResponseExtractor,
) -> Result<String, ResponseExtractionError> {
    let value = match extractor {
        ResponseExtractor::JsonPath { path } => extract_json_path(response, path)?,
        ResponseExtractor::Header { name } => extract_header(response, name)?,
        ResponseExtractor::PlainBody => extract_plain_body(response)?,
        ResponseExtractor::Status => response.status.to_string(),
    };
    ensure_value_size(&value)?;
    Ok(value)
}

/// Extracts a stage's captures into the template-ready capture map.
///
/// Capture values are returned to the caller because later stages may need
/// them in templates.  They are never included in an error message.
pub fn extract_captures(
    response: &ResponseData<'_>,
    captures: &[Capture],
) -> Result<BTreeMap<String, String>, ResponseExtractionError> {
    if captures.len() > MAX_CAPTURE_COUNT {
        return Err(ResponseExtractionError::TooManyCaptures);
    }

    let mut values = BTreeMap::new();
    let mut total_bytes = 0usize;
    for capture in captures {
        if values.contains_key(&capture.id) {
            return Err(ResponseExtractionError::DuplicateCapture {
                id: capture.id.clone(),
            });
        }
        let value = extract(response, &capture.from).map_err(|source| {
            ResponseExtractionError::Capture {
                id: capture.id.clone(),
                source: Box::new(source),
            }
        })?;
        total_bytes = total_bytes
            .checked_add(value.len())
            .ok_or(ResponseExtractionError::CapturesTooLarge)?;
        if total_bytes > MAX_CAPTURE_BYTES {
            return Err(ResponseExtractionError::CapturesTooLarge);
        }
        values.insert(capture.id.clone(), value);
    }
    Ok(values)
}

fn extract_json_path(
    response: &ResponseData<'_>,
    path: &str,
) -> Result<String, ResponseExtractionError> {
    ensure_body_size(response.body)?;
    let path = crate::jsonpath::parse_text_path(path)
        .map_err(|_| ResponseExtractionError::InvalidJsonPath)?;
    crate::jsonpath::extract_text_from_response(response.body, &path).map_err(map_json_error)
}

fn extract_header(
    response: &ResponseData<'_>,
    name: &str,
) -> Result<String, ResponseExtractionError> {
    let name = HeaderName::from_bytes(name.as_bytes())
        .map_err(|_| ResponseExtractionError::InvalidHeaderName)?;
    let name_text = name.as_str().to_owned();
    let value =
        response
            .headers
            .get(&name)
            .ok_or_else(|| ResponseExtractionError::HeaderMissing {
                name: name_text.clone(),
            })?;
    if value.as_bytes().len() > MAX_EXTRACTED_VALUE_BYTES {
        return Err(ResponseExtractionError::HeaderTooLarge { name: name_text });
    }
    value
        .to_str()
        .map(str::to_owned)
        .map_err(|_| ResponseExtractionError::HeaderNotUtf8 { name: name_text })
}

fn extract_plain_body(response: &ResponseData<'_>) -> Result<String, ResponseExtractionError> {
    ensure_body_size(response.body)?;
    std::str::from_utf8(response.body)
        .map(str::to_owned)
        .map_err(|_| ResponseExtractionError::InvalidBodyUtf8)
}

fn ensure_body_size(body: &[u8]) -> Result<(), ResponseExtractionError> {
    if body.len() > MAX_RESPONSE_BODY_BYTES {
        Err(ResponseExtractionError::ResponseTooLarge)
    } else {
        Ok(())
    }
}

fn ensure_value_size(value: &str) -> Result<(), ResponseExtractionError> {
    if value.len() > MAX_EXTRACTED_VALUE_BYTES {
        Err(ResponseExtractionError::ValueTooLarge)
    } else {
        Ok(())
    }
}

fn map_json_error(error: crate::jsonpath::TextExtractionError) -> ResponseExtractionError {
    match error {
        crate::jsonpath::TextExtractionError::InvalidJson(error) => {
            ResponseExtractionError::InvalidJson {
                line: error.line(),
                column: error.column(),
            }
        }
        crate::jsonpath::TextExtractionError::NoMatch => ResponseExtractionError::JsonPathNoMatch,
        crate::jsonpath::TextExtractionError::MultipleMatches { count } => {
            ResponseExtractionError::JsonPathMultipleMatches { count }
        }
        crate::jsonpath::TextExtractionError::InvalidType { kind } => {
            ResponseExtractionError::JsonPathInvalidType { kind }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response<'a>(status: u16, headers: &'a HeaderMap, body: &'a [u8]) -> ResponseData<'a> {
        ResponseData::new(status, headers, body)
    }

    #[test]
    fn extracts_json_path_scalars_with_legacy_semantics() {
        let headers = HeaderMap::new();
        let response = response(200, &headers, br#"{"text":"hello","number":7,"flag":true}"#);

        for (path, expected) in [("$.text", "hello"), ("$.number", "7"), ("$.flag", "true")] {
            assert_eq!(
                extract(
                    &response,
                    &ResponseExtractor::JsonPath { path: path.into() }
                )
                .unwrap(),
                expected
            );
        }
    }

    #[test]
    fn json_path_requires_exactly_one_scalar_without_exposing_the_body() {
        let headers = HeaderMap::new();
        let response = response(200, &headers, br#"{"items":["a","b"],"object":{}}"#);

        assert!(matches!(
            extract(
                &response,
                &ResponseExtractor::JsonPath {
                    path: "$.missing".into(),
                }
            ),
            Err(ResponseExtractionError::JsonPathNoMatch)
        ));
        assert!(matches!(
            extract(
                &response,
                &ResponseExtractor::JsonPath {
                    path: "$.items[*]".into(),
                }
            ),
            Err(ResponseExtractionError::JsonPathMultipleMatches { count: 2 })
        ));
        assert!(matches!(
            extract(
                &response,
                &ResponseExtractor::JsonPath {
                    path: "$.object".into(),
                }
            ),
            Err(ResponseExtractionError::JsonPathInvalidType { kind: "an object" })
        ));
    }

    #[test]
    fn extracts_headers_case_insensitively_plain_body_and_status() {
        let mut headers = HeaderMap::new();
        headers.insert("X-Task-ID", "job-42".parse().unwrap());
        let response = response(202, &headers, b"plain transcript");

        assert_eq!(
            extract(
                &response,
                &ResponseExtractor::Header {
                    name: "x-tAsK-iD".into(),
                },
            )
            .unwrap(),
            "job-42"
        );
        assert_eq!(
            extract(&response, &ResponseExtractor::PlainBody).unwrap(),
            "plain transcript"
        );
        assert_eq!(
            extract(&response, &ResponseExtractor::Status).unwrap(),
            "202"
        );
    }

    #[test]
    fn captures_are_template_ready_and_attribute_failures_without_values() {
        let mut headers = HeaderMap::new();
        headers.insert("X-Upload", "upload-7".parse().unwrap());
        let response = response(201, &headers, br#"{"job":"job-9"}"#);
        let captures = [
            Capture {
                id: "job_id".into(),
                from: ResponseExtractor::JsonPath {
                    path: "$.job".into(),
                },
                sensitive: false,
            },
            Capture {
                id: "upload_id".into(),
                from: ResponseExtractor::Header {
                    name: "x-upload".into(),
                },
                sensitive: true,
            },
            Capture {
                id: "status".into(),
                from: ResponseExtractor::Status,
                sensitive: false,
            },
        ];

        let values = extract_captures(&response, &captures).unwrap();
        assert_eq!(values.get("job_id").map(String::as_str), Some("job-9"));
        assert_eq!(
            values.get("upload_id").map(String::as_str),
            Some("upload-7")
        );
        assert_eq!(values.get("status").map(String::as_str), Some("201"));

        let missing = [Capture {
            id: "token".into(),
            from: ResponseExtractor::Header {
                name: "x-missing".into(),
            },
            sensitive: true,
        }];
        assert_eq!(
            extract_captures(&response, &missing)
                .unwrap_err()
                .to_string(),
            "failed to extract capture 'token': response header 'x-missing' was not present"
        );
    }

    #[test]
    fn errors_never_include_response_body_or_header_values() {
        let mut headers = HeaderMap::new();
        headers.insert("X-Secret", "header-secret-value".parse().unwrap());
        let response = response(200, &headers, br#"{"secret":"body-secret-value"}"#);

        let json_error = extract(
            &response,
            &ResponseExtractor::JsonPath {
                path: "$.missing".into(),
            },
        )
        .unwrap_err()
        .to_string();
        let header_error = extract(
            &response,
            &ResponseExtractor::Header {
                name: "x-missing".into(),
            },
        )
        .unwrap_err()
        .to_string();
        assert!(!json_error.contains("body-secret-value"));
        assert!(!header_error.contains("header-secret-value"));
    }

    #[test]
    fn rejects_invalid_data_and_runtime_limit_violations() {
        let headers = HeaderMap::new();
        let invalid_json = response(200, &headers, b"not-json-secret");
        let error = extract(
            &invalid_json,
            &ResponseExtractor::JsonPath {
                path: "$.text".into(),
            },
        )
        .unwrap_err();
        assert!(matches!(error, ResponseExtractionError::InvalidJson { .. }));
        assert!(!error.to_string().contains("not-json-secret"));

        let invalid_body = response(200, &headers, &[0xFF]);
        assert!(matches!(
            extract(&invalid_body, &ResponseExtractor::PlainBody),
            Err(ResponseExtractionError::InvalidBodyUtf8)
        ));

        let large_body = vec![b'x'; MAX_RESPONSE_BODY_BYTES + 1];
        let too_large = response(200, &headers, &large_body);
        assert!(matches!(
            extract(&too_large, &ResponseExtractor::PlainBody),
            Err(ResponseExtractionError::ResponseTooLarge)
        ));

        let many = vec![
            Capture {
                id: "capture".into(),
                from: ResponseExtractor::Status,
                sensitive: false,
            };
            MAX_CAPTURE_COUNT + 1
        ];
        assert!(matches!(
            extract_captures(&response(200, &headers, b""), &many),
            Err(ResponseExtractionError::TooManyCaptures)
        ));
    }

    #[test]
    fn rejects_duplicate_capture_ids() {
        let headers = HeaderMap::new();
        let response = response(200, &headers, b"");
        let captures = [
            Capture {
                id: "id".into(),
                from: ResponseExtractor::Status,
                sensitive: false,
            },
            Capture {
                id: "id".into(),
                from: ResponseExtractor::Status,
                sensitive: false,
            },
        ];

        assert!(matches!(
            extract_captures(&response, &captures),
            Err(ResponseExtractionError::DuplicateCapture { .. })
        ));
    }
}
