use serde_json::Value;
use serde_json_path::{JsonPath, ParseError};
use thiserror::Error;

#[derive(Debug, Error)]
#[error("invalid TEXT_PATH: {0}")]
pub struct TextPathError(#[from] ParseError);

#[derive(Debug, Error)]
pub enum TextExtractionError {
    #[error("ASR response is not valid JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("TEXT_PATH matched no values; expected exactly one")]
    NoMatch,
    #[error("TEXT_PATH matched {count} values; expected exactly one")]
    MultipleMatches { count: usize },
    #[error("TEXT_PATH selected {kind}; expected a string, number or boolean")]
    InvalidType { kind: &'static str },
}

pub fn parse_text_path(path: &str) -> Result<JsonPath, TextPathError> {
    Ok(JsonPath::parse(path)?)
}

pub fn extract_text_from_response(
    body: &[u8],
    text_path: &JsonPath,
) -> Result<String, TextExtractionError> {
    let root: Value = serde_json::from_slice(body)?;
    let nodes = text_path.query(&root);
    let count = nodes.len();
    let value = nodes.exactly_one().map_err(|_| {
        if count == 0 {
            TextExtractionError::NoMatch
        } else {
            TextExtractionError::MultipleMatches { count }
        }
    })?;
    match value {
        Value::String(text) => Ok(text.clone()),
        Value::Number(number) => Ok(number.to_string()),
        Value::Bool(value) => Ok(value.to_string()),
        Value::Null => Err(TextExtractionError::InvalidType { kind: "null" }),
        Value::Array(_) => Err(TextExtractionError::InvalidType { kind: "an array" }),
        Value::Object(_) => Err(TextExtractionError::InvalidType { kind: "an object" }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(body: &str, path: &str) -> Result<String, TextExtractionError> {
        extract_text_from_response(body.as_bytes(), &parse_text_path(path).unwrap())
    }

    #[test]
    fn standard_selectors_and_scalar_conversion() {
        let body = r#"{
            "text": "hello", "result": {"transcript": "nested"},
            "results": [{"alternatives": [{"transcript": true}]}],
            "data": {"items": [[0, {"text": 7.5}]]},
            "segments": [{"id": 1, "text": "first"}, {"id": 42, "text": "last"}],
            "result.text": "dotted", "recognition result": {"text-value": "special"}
        }"#;
        for (path, expected) in [
            ("$.text", "hello"),
            ("$.result.transcript", "nested"),
            ("$.results[0].alternatives[0].transcript", "true"),
            ("$.data.items[0][1].text", "7.5"),
            ("$.segments[0].text", "first"),
            ("$.segments[-1].text", "last"),
            ("$.segments[?@.id == 42].text", "last"),
            ("$['result.text']", "dotted"),
            ("$['recognition result']['text-value']", "special"),
        ] {
            assert_eq!(extract(body, path).unwrap(), expected, "{path}");
        }
        assert_eq!(extract(r#""root""#, "$").unwrap(), "root");
        assert_eq!(extract(r#"["root"]"#, "$[0]").unwrap(), "root");
    }

    #[test]
    fn requires_exactly_one_match_without_fallback() {
        let body =
            r#"{"text":"fallback","other":"also fallback","segments":[{"text":"a"},{"text":"b"}]}"#;
        assert!(matches!(
            extract(body, "$.missing"),
            Err(TextExtractionError::NoMatch)
        ));
        for path in ["$.segments[*].text", "$.segments[0:2].text"] {
            assert!(matches!(
                extract(body, path),
                Err(TextExtractionError::MultipleMatches { count: 2 })
            ));
        }
        assert!(matches!(
            extract(body, "$..text"),
            Err(TextExtractionError::MultipleMatches { count: 3 })
        ));
        // [0] indexes each selected JSON value, not the query's result list.
        assert!(matches!(
            extract(body, "$.segments[*].text[0]"),
            Err(TextExtractionError::NoMatch)
        ));
        for path in ["$.segments[*].text", "$.segments[0:2].text", "$..text"] {
            assert_eq!(
                extract(r#"{"segments":[{"text":"only"}]}"#, path).unwrap(),
                "only"
            );
        }
    }

    #[test]
    fn rejects_non_scalar_values_but_accepts_empty_strings() {
        for (body, kind) in [
            (r#"{"text":null}"#, "null"),
            (r#"{"text":[]}"#, "an array"),
            (r#"{"text":{}}"#, "an object"),
        ] {
            assert!(
                matches!(extract(body, "$.text"), Err(TextExtractionError::InvalidType { kind: actual }) if actual == kind)
            );
        }
        assert_eq!(extract(r#"{"text":""}"#, "$.text").unwrap(), "");
        assert!(matches!(
            extract("not JSON", "$.text"),
            Err(TextExtractionError::InvalidJson(_))
        ));
    }

    #[test]
    fn rejects_empty_legacy_and_malformed_paths() {
        for path in ["", "text", "result.transcript", "$.items[", "$.items[bad]"] {
            assert!(
                parse_text_path(path)
                    .unwrap_err()
                    .to_string()
                    .starts_with("invalid TEXT_PATH:"),
                "{path}"
            );
        }
    }
}
