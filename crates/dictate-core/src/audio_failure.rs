//! Safe, user-visible placeholders for terminal audio request failures.
//!
//! These strings are inserted into a focused application, so they must remain
//! concise and must never contain an unbounded server response or credentials.

use reqwest::StatusCode;
use serde_json::Value;

const MAX_DETAIL_CHARS: usize = 160;

pub(crate) fn placeholder(reason: &str) -> String {
    format!("[{reason}]")
}

pub(crate) fn http_placeholder(status: u16, response: &[u8], config: &crate::Config) -> String {
    let status_text = StatusCode::from_u16(status)
        .ok()
        .and_then(|status| status.canonical_reason().map(|_| status.to_string()))
        .unwrap_or_else(|| format!("HTTP {status}"));
    let Some(detail) = response_detail(response, config) else {
        return placeholder(&status_text);
    };
    placeholder(&format!("{status_text}: {detail}"))
}

pub(crate) fn response_detail(response: &[u8], config: &crate::Config) -> Option<String> {
    let response = serde_json::from_slice::<Value>(response).ok()?;
    let value = response_error_value(&response)?;
    let value = crate::debug_log::request_message(value, config);
    if value.trim() == "[redacted]" {
        return None;
    }
    let value = compact_detail(&value);
    (!value.is_empty()).then_some(value)
}

fn response_error_value(response: &Value) -> Option<&str> {
    let object = response.as_object()?;
    if let Some(error) = object.get("error") {
        if let Some(value) = error.as_str() {
            return Some(value);
        }
        if let Some(error) = error.as_object() {
            for field in ["message", "info", "detail"] {
                if let Some(value) = error.get(field).and_then(Value::as_str) {
                    return Some(value);
                }
            }
        }
    }
    for field in ["message", "info", "detail"] {
        if let Some(value) = object.get(field).and_then(Value::as_str) {
            return Some(value);
        }
    }
    None
}

fn compact_detail(value: &str) -> String {
    let value = value
        .chars()
        .map(|character| {
            if character.is_control() || matches!(character, '[' | ']') {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut characters = value.chars();
    let mut result = characters
        .by_ref()
        .take(MAX_DETAIL_CHARS)
        .collect::<String>();
    if characters.next().is_some() {
        result.push('…');
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_placeholders_use_status_and_a_bounded_json_detail() {
        let config = crate::Config {
            token: "private-token".into(),
            ..crate::Config::default()
        };
        assert_eq!(
            http_placeholder(503, br#"{"error":{"info":"please retry later"}}"#, &config,),
            "[503 Service Unavailable: please retry later]"
        );
        assert_eq!(
            http_placeholder(599, br#"{"message":"private-token"}"#, &config),
            "[HTTP 599]"
        );
    }

    #[test]
    fn response_details_do_not_use_unstructured_bodies() {
        let config = crate::Config::default();
        assert_eq!(response_detail(b"temporary proxy error", &config), None);
        assert_eq!(
            http_placeholder(502, b"temporary proxy error", &config),
            "[502 Bad Gateway]"
        );
    }
}
