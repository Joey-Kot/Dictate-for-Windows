//! Provider wire formats and bounded, cancellable text requests.
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::{Config, additional_parameters};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    #[default]
    OpenaiCompatible,
    OpenaiResponses,
    OpenaiCompletions,
    Google,
    Anthropic,
    Deepseek,
    Qwen,
    Glm,
}

impl Provider {
    pub const ALL: [Self; 8] = [
        Self::OpenaiCompatible,
        Self::OpenaiResponses,
        Self::OpenaiCompletions,
        Self::Google,
        Self::Anthropic,
        Self::Deepseek,
        Self::Qwen,
        Self::Glm,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::OpenaiCompatible => "OpenAI-Compatible",
            Self::OpenaiResponses => "OpenAI Responses",
            Self::OpenaiCompletions => "OpenAI Completions",
            Self::Google => "Google",
            Self::Anthropic => "Anthropic",
            Self::Deepseek => "DeepSeek",
            Self::Qwen => "Qwen",
            Self::Glm => "GLM",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RewritePrompt {
    pub id: String,
    pub title: String,
    pub prompt: String,
    pub extra_config: String,
    pub hotkey: String,
}

impl Default for RewritePrompt {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            title: String::new(),
            prompt: String::new(),
            extra_config: String::new(),
            hotkey: String::new(),
        }
    }
}

impl RewritePrompt {
    pub fn validate(&self) -> Result<(), String> {
        if self.id.is_empty() || self.title.trim().is_empty() || self.prompt.trim().is_empty() {
            return Err("Prompt ID, title and content must not be empty".into());
        }
        crate::hotkey::parse_hotkey(&self.hotkey).map_err(|e| format!("{}: {e}", self.title))?;
        additional_parameters::parse(&self.extra_config)
            .map_err(|e| format!("{}: {e}", self.title))?;
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RewriteConfig {
    pub provider: Provider,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub prompts: Vec<RewritePrompt>,
}

impl RewriteConfig {
    pub fn validate(&self) -> Result<(), String> {
        let mut ids = std::collections::HashSet::new();
        for prompt in &self.prompts {
            prompt.validate()?;
            if !ids.insert(&prompt.id) {
                return Err("Duplicate prompt ID".into());
            }
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RewriteError {
    #[error("Request canceled")]
    Canceled,
    #[error("{0}")]
    Configuration(String),
    #[error("{0}")]
    Response(String),
    #[error("{message}")]
    Request { message: String, retryable: bool },
}

pub struct Request {
    pub url: reqwest::Url,
    pub headers: Vec<(&'static str, String)>,
    pub body: Value,
}

pub fn build_request(
    config: &RewriteConfig,
    prompt: &RewritePrompt,
    input: &str,
) -> Result<Request, RewriteError> {
    use RewriteError::Configuration;
    if config.api_key.trim().is_empty() {
        return Err(Configuration("Rewrite API key is empty".into()));
    }
    if prompt.prompt.trim().is_empty() || input.is_empty() {
        return Err(Configuration(
            "Prompt and selected text must not be empty".into(),
        ));
    }
    let mut base = json!({"model":config.model.trim()});
    match config.provider {
        Provider::OpenaiResponses => {
            base["instructions"] = json!(prompt.prompt);
            base["input"] = json!([{"role":"user","content":input}]);
        }
        Provider::Google => {
            base["systemInstruction"] = json!({"parts":[{"text":prompt.prompt}]});
            base["contents"] = json!([{"role":"user","parts":[{"text":input}]}]);
        }
        Provider::Anthropic => {
            base["system"] = json!(prompt.prompt);
            base["max_tokens"] = json!(4096);
            base["messages"] = json!([{"role":"user","content":input}]);
        }
        _ => {
            base["messages"] =
                json!([{"role":"system","content":prompt.prompt},{"role":"user","content":input}])
        }
    }
    let extra = additional_parameters::parse(&prompt.extra_config)
        .map_err(|e| Configuration(e.to_string()))?;
    let mut body = Value::Object(additional_parameters::merge(
        base.as_object().unwrap(),
        &extra,
    ));
    let model = body["model"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| Configuration("Merged model must be a nonempty string".into()))?;
    let url = endpoint(&config.base_url, config.provider, model)?;
    let headers = match config.provider {
        Provider::Anthropic => vec![
            ("x-api-key", config.api_key.clone()),
            ("anthropic-version", "2023-06-01".into()),
        ],
        Provider::Google => vec![("x-goog-api-key", config.api_key.clone())],
        _ => vec![("Authorization", format!("Bearer {}", config.api_key))],
    };
    if config.provider == Provider::Google {
        body.as_object_mut().unwrap().remove("model");
    }
    Ok(Request { url, headers, body })
}

fn endpoint(base: &str, provider: Provider, model: &str) -> Result<reqwest::Url, RewriteError> {
    let invalid = || RewriteError::Configuration("Invalid Rewrite Base URL".into());
    let mut url = reqwest::Url::parse(base.trim()).map_err(|_| invalid())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid());
    }
    let mut path = url.path().trim_end_matches('/').to_owned();
    if provider == Provider::Google {
        let model = model.strip_prefix("models/").unwrap_or(model);
        if model.is_empty() || model.contains(['/', '?', '#']) {
            return Err(RewriteError::Configuration(
                "Invalid Google model name".into(),
            ));
        }
        path = path
            .split("/models/")
            .next()
            .unwrap_or("")
            .trim_end_matches('/')
            .to_owned();
        if path.is_empty() {
            path = "/v1beta".into();
        }
        path.push_str(&format!("/models/{model}:generateContent"));
    } else {
        let suffix = match provider {
            Provider::OpenaiResponses => "/responses",
            Provider::Anthropic => "/messages",
            _ => "/chat/completions",
        };
        if path.is_empty() {
            path = match provider {
                Provider::Deepseek => "",
                Provider::Qwen => "/compatible-mode/v1",
                Provider::Glm => "/api/paas/v4",
                _ => "/v1",
            }
            .into();
        }
        if !path.ends_with(suffix) {
            path.push_str(suffix);
        }
    }
    url.set_path(&path);
    Ok(url)
}

fn blocks(value: &Value, allowed: &str) -> String {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter(|v| v["type"].as_str() == Some(allowed))
        .filter_map(|v| v["text"].as_str())
        .collect()
}

fn validate_finish_reason(provider: Provider, reason: &Value) -> Result<(), RewriteError> {
    let Some(reason) = reason.as_str() else {
        return Ok(());
    };
    // Some compatible services omit the reason or add their own values. Reject the
    // documented nonfinal outcomes without requiring every service to use "stop".
    let incomplete = match provider {
        Provider::Anthropic => matches!(
            reason,
            "max_tokens" | "tool_use" | "pause_turn" | "refusal" | "model_context_window_exceeded"
        ),
        Provider::Google => matches!(
            reason,
            "MAX_TOKENS"
                | "SAFETY"
                | "RECITATION"
                | "LANGUAGE"
                | "OTHER"
                | "BLOCKLIST"
                | "PROHIBITED_CONTENT"
                | "SPII"
                | "MALFORMED_FUNCTION_CALL"
                | "UNEXPECTED_TOOL_CALL"
                | "TOO_MANY_TOOL_CALLS"
                | "MISSING_THOUGHT_SIGNATURE"
                | "MALFORMED_RESPONSE"
                | "ESCALATION"
                | "PUP_LIMITED_DISABLED"
                | "IMAGE_SAFETY"
                | "IMAGE_PROHIBITED_CONTENT"
                | "IMAGE_OTHER"
                | "NO_IMAGE"
                | "IMAGE_RECITATION"
        ),
        Provider::OpenaiResponses => false,
        _ => matches!(
            reason,
            "length"
                | "content_filter"
                | "tool_calls"
                | "function_call"
                | "insufficient_system_resource"
                | "aborted"
        ),
    };
    if incomplete {
        Err(RewriteError::Response(format!(
            "Rewrite response is not complete ({}: {reason})",
            provider.label()
        )))
    } else {
        Ok(())
    }
}

pub fn parse_text(provider: Provider, value: &Value) -> Result<String, RewriteError> {
    if value.get("error").is_some_and(|error| !error.is_null()) || value["type"] == "error" {
        return Err(stream_error(value));
    }
    if provider == Provider::OpenaiResponses
        && matches!(
            value["status"].as_str(),
            Some("failed" | "incomplete" | "cancelled" | "in_progress" | "queued")
        )
    {
        return Err(RewriteError::Response(
            "Rewrite response is not complete".into(),
        ));
    }
    let reason = match provider {
        Provider::Anthropic => &value["stop_reason"],
        Provider::Google => &value["candidates"][0]["finishReason"],
        _ => &value["choices"][0]["finish_reason"],
    };
    validate_finish_reason(provider, reason)?;
    let text = match provider {
        Provider::OpenaiResponses => value["output_text"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| {
                value["output"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|v| v["type"] == "message")
                    .map(|v| blocks(&v["content"], "output_text"))
                    .collect()
            }),
        Provider::Anthropic => blocks(&value["content"], "text"),
        Provider::Google => value["candidates"][0]["content"]["parts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|v| v["thought"] != true)
            .filter_map(|v| v["text"].as_str())
            .collect(),
        _ => {
            let content = &value["choices"][0]["message"]["content"];
            content
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| blocks(content, "text"))
        }
    };
    if text.trim().is_empty() {
        Err(RewriteError::Response(
            "Rewrite response contains no text".into(),
        ))
    } else {
        Ok(text)
    }
}

fn stream_error(value: &Value) -> RewriteError {
    let error = value
        .get("error")
        .or_else(|| value["response"].get("error"))
        .unwrap_or(value);
    let code = error
        .get("code")
        .filter(|v| v.is_string() || v.is_number())
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| v.to_string())
        })
        .or_else(|| error["type"].as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown_error".into());
    let status = ["status_code", "status", "code"]
        .into_iter()
        .filter_map(|key| {
            error[key]
                .as_u64()
                .or_else(|| error[key].as_str().and_then(|s| s.parse::<u64>().ok()))
        })
        .find(|s| (400..=599).contains(s));
    let retryable = status.is_some_and(|s| s == 408 || s == 429 || s >= 500)
        || matches!(
            code.as_str(),
            "overloaded_error"
                | "rate_limit_error"
                | "rate_limit_exceeded"
                | "api_error"
                | "server_error"
                | "timeout_error"
        );
    RewriteError::Request {
        message: format!(
            "Rewrite stream error ({code}): {}",
            error["message"].as_str().unwrap_or("Service error")
        ),
        retryable,
    }
}

pub fn parse_stream(provider: Provider, response: &str) -> Result<String, RewriteError> {
    let mut text = String::new();
    let mut completed = false;
    let mut final_text = None;
    let mut name = String::new();
    let mut data = Vec::new();
    for line in response
        .trim_start_matches('\u{feff}')
        .lines()
        .chain(std::iter::once(""))
    {
        if let Some(value) = line.strip_prefix("event:") {
            name = value.trim_start_matches(' ').to_owned();
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push(value.trim_start_matches(' '));
        } else if line.is_empty() {
            if data.is_empty() {
                name.clear();
                continue;
            }
            let payload = data.join("\n");
            data.clear();
            if payload == "[DONE]" {
                completed = true;
                name.clear();
                continue;
            }
            let root: Value = serde_json::from_str(&payload)
                .map_err(|e| RewriteError::Response(format!("Invalid rewrite stream: {e}")))?;
            let kind = root["type"].as_str().unwrap_or(&name);
            if root.get("error").is_some() || matches!(kind, "error" | "response.failed") {
                return Err(stream_error(&root));
            }
            if kind == "response.incomplete" {
                return Err(RewriteError::Response("Incomplete rewrite stream".into()));
            }
            match provider {
                Provider::OpenaiResponses => match kind {
                    "response.output_text.delta" => {
                        text.push_str(root["delta"].as_str().unwrap_or(""))
                    }
                    "response.completed" => {
                        completed = true;
                        if root["response"].get("output").is_some()
                            || root["response"].get("output_text").is_some()
                        {
                            final_text = Some(parse_text(provider, &root["response"])?);
                        }
                    }
                    _ => {}
                },
                Provider::Anthropic => match kind {
                    "content_block_start" if root["content_block"]["type"] == "text" => {
                        text.push_str(root["content_block"]["text"].as_str().unwrap_or(""))
                    }
                    "content_block_delta" if root["delta"]["type"] == "text_delta" => {
                        text.push_str(root["delta"]["text"].as_str().unwrap_or(""))
                    }
                    "message_delta" => {
                        validate_finish_reason(provider, &root["delta"]["stop_reason"])?;
                    }
                    "message_stop" => completed = true,
                    _ => {}
                },
                Provider::Google => {
                    let candidate = &root["candidates"][0];
                    validate_finish_reason(provider, &candidate["finishReason"])?;
                    for part in candidate["content"]["parts"]
                        .as_array()
                        .into_iter()
                        .flatten()
                    {
                        if part["thought"] != true {
                            text.push_str(part["text"].as_str().unwrap_or(""));
                        }
                    }
                    if candidate["finishReason"]
                        .as_str()
                        .is_some_and(|s| !s.is_empty() && s != "FINISH_REASON_UNSPECIFIED")
                    {
                        completed = true;
                    }
                }
                _ => {
                    for choice in root["choices"].as_array().into_iter().flatten() {
                        if choice["index"].as_u64().unwrap_or(0) != 0 {
                            continue;
                        }
                        validate_finish_reason(provider, &choice["finish_reason"])?;
                        text.push_str(choice["delta"]["content"].as_str().unwrap_or(""));
                        if choice.get("finish_reason").is_some_and(|v| !v.is_null()) {
                            completed = true;
                        }
                    }
                }
            }
            name.clear();
        }
    }
    let text = final_text.unwrap_or(text);
    if !completed || text.trim().is_empty() {
        return Err(RewriteError::Response(
            "Rewrite stream ended without a complete text result".into(),
        ));
    }
    Ok(text)
}

pub struct RewriteClient {
    config: Config,
    client: reqwest::Client,
}

impl RewriteClient {
    pub fn new(config: Config) -> Result<Self, RewriteError> {
        let client = crate::network::client(&config)
            .map_err(|e| RewriteError::Configuration(e.to_string()))?;
        Ok(Self { config, client })
    }

    pub async fn test_connection(&self, cancel: &CancellationToken) -> Result<(), RewriteError> {
        self.debug(format_args!("[upload] Rewrite connectivity test"));
        let prompt = RewritePrompt {
            prompt: "Reply briefly with OK.".into(),
            ..Default::default()
        };
        self.execute(&prompt, "Connection test.", cancel, false)
            .await
            .map(|_| ())
    }

    pub async fn execute(
        &self,
        prompt: &RewritePrompt,
        input: &str,
        cancel: &CancellationToken,
        retry: bool,
    ) -> Result<String, RewriteError> {
        let request = match build_request(&self.config.rewrite, prompt, input) {
            Ok(request) => request,
            Err(error) => {
                self.debug(format_args!(
                    "[upload] Rewrite configuration error: {error}"
                ));
                return Err(error);
            }
        };
        let attempts = if retry {
            self.config.max_retry.max(1)
        } else {
            1
        };
        let mut delay = self.config.retry_base_delay.max(0.0);
        for attempt in 1..=attempts {
            if cancel.is_cancelled() {
                self.debug(format_args!("[upload] Rewrite canceled before request"));
                return Err(RewriteError::Canceled);
            }
            let started = std::time::Instant::now();
            self.debug(format_args!(
                "[upload] Rewrite {} attempt {attempt}/{attempts}: POST {}",
                self.config.rewrite.provider.label(),
                crate::debug_log::safe_url(request.url.as_str()),
            ));
            let result = self
                .once(&request, cancel)
                .await
                .map_err(|error| self.redact(error));
            match &result {
                Ok(text) => self.debug(format_args!(
                    "[upload] Rewrite completed, characters={}, elapsed={}ms",
                    text.chars().count(),
                    started.elapsed().as_millis()
                )),
                Err(error) => self.debug(format_args!(
                    "[upload] Rewrite attempt {attempt} failed, elapsed={}ms: {error}",
                    started.elapsed().as_millis()
                )),
            }
            match result {
                Err(RewriteError::Request {
                    retryable: true, ..
                }) if attempt < attempts => {
                    self.debug(format_args!(
                        "[upload] Rewrite retry in {:.3}s",
                        delay.min(86400.0)
                    ));
                    tokio::select! {
                        _ = cancel.cancelled() => {
                            self.debug(format_args!("[upload] Rewrite canceled during retry wait"));
                            return Err(RewriteError::Canceled);
                        },
                        _ = tokio::time::sleep(Duration::from_secs_f64(delay.min(86400.0))) => {}
                    }
                    delay *= 2.0;
                }
                result => return result,
            }
        }
        unreachable!()
    }

    async fn once(
        &self,
        request: &Request,
        cancel: &CancellationToken,
    ) -> Result<String, RewriteError> {
        let operation = async {
            let mut builder = self
                .client
                .post(request.url.clone())
                .header("Content-Type", "application/json")
                .header("User-Agent", "stt-go-client/1.0")
                .body(request.body.to_string());
            for (key, value) in &request.headers {
                builder = builder.header(*key, value);
            }
            let mut response = builder.send().await.map_err(|e| self.network_error(e))?;
            let status = response.status();
            let stream = response
                .headers()
                .get("content-type")
                .and_then(|h| h.to_str().ok())
                .is_some_and(|h| h.to_ascii_lowercase().contains("text/event-stream"));
            self.debug(format_args!("[upload] Rewrite HTTP {status}, SSE={stream}"));
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|e| self.network_error(e))? {
                if bytes.len() + chunk.len() > 2 * 1024 * 1024 {
                    return Err(RewriteError::Response(
                        "Rewrite response exceeds 2 MiB".into(),
                    ));
                }
                bytes.extend_from_slice(&chunk);
            }
            let body = String::from_utf8(bytes)
                .map_err(|_| RewriteError::Response("Rewrite response is not UTF-8".into()))?;
            if !status.is_success() {
                let summary: String = crate::debug_log::request_message(&body, &self.config)
                    .chars()
                    .take(500)
                    .collect();
                return Err(RewriteError::Request {
                    message: format!(
                        "HTTP {status}: {}",
                        summary.replace(&self.config.rewrite.api_key, "[redacted]")
                    ),
                    retryable: matches!(status.as_u16(), 408 | 429 | 500..=599),
                });
            }
            if stream {
                parse_stream(self.config.rewrite.provider, &body)
            } else {
                parse_text(
                    self.config.rewrite.provider,
                    &serde_json::from_str(&body).map_err(|e| {
                        RewriteError::Response(format!("Invalid rewrite JSON: {e}"))
                    })?,
                )
            }
        };
        tokio::select! { biased;
            _ = cancel.cancelled() => Err(RewriteError::Canceled),
            result = operation => result,
        }
    }

    fn redact(&self, error: RewriteError) -> RewriteError {
        let clean = |message: String| {
            crate::debug_log::request_message(&message, &self.config)
                .chars()
                .take(800)
                .collect()
        };
        match error {
            RewriteError::Request { message, retryable } => RewriteError::Request {
                message: clean(message),
                retryable,
            },
            RewriteError::Response(message) => RewriteError::Response(clean(message)),
            error => error,
        }
    }

    fn debug(&self, message: std::fmt::Arguments<'_>) {
        if self.config.upload_debug {
            let message = crate::debug_log::request_message(&message.to_string(), &self.config);
            crate::debug_log::write(
                crate::debug_log::Category::Upload,
                format_args!("{message}"),
            );
        }
    }

    fn network_error(&self, error: reqwest::Error) -> RewriteError {
        RewriteError::Request {
            message: error.to_string(),
            retryable: !error.is_builder(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(provider: Provider) -> RewriteConfig {
        RewriteConfig {
            provider,
            base_url: "https://example.com".into(),
            api_key: "key".into(),
            model: "default".into(),
            prompts: vec![],
        }
    }

    #[test]
    fn all_providers_use_expected_routes_headers_and_payloads() {
        for (provider, path, header) in [
            (
                Provider::OpenaiCompatible,
                "/v1/chat/completions",
                "Authorization",
            ),
            (
                Provider::OpenaiCompletions,
                "/v1/chat/completions",
                "Authorization",
            ),
            (Provider::OpenaiResponses, "/v1/responses", "Authorization"),
            (Provider::Anthropic, "/v1/messages", "x-api-key"),
            (
                Provider::Google,
                "/v1beta/models/override:generateContent",
                "x-goog-api-key",
            ),
            (Provider::Deepseek, "/chat/completions", "Authorization"),
            (
                Provider::Qwen,
                "/compatible-mode/v1/chat/completions",
                "Authorization",
            ),
            (
                Provider::Glm,
                "/api/paas/v4/chat/completions",
                "Authorization",
            ),
        ] {
            let prompt = RewritePrompt {
                prompt: "Translate".into(),
                extra_config: r#"{"model":"override"}"#.into(),
                ..Default::default()
            };
            let request = build_request(&config(provider), &prompt, "hello").unwrap();
            assert_eq!(request.url.path(), path);
            assert_eq!(request.headers[0].0, header);
            if provider == Provider::Google {
                assert!(request.body.get("model").is_none());
            } else {
                assert_eq!(request.body["model"], "override");
            }
            assert!(request.body.to_string().contains("hello"));
            assert!(request.body.to_string().contains("Translate"));
        }
    }

    #[test]
    fn rejects_invalid_merged_model_and_does_not_duplicate_suffix() {
        let prompt = RewritePrompt {
            prompt: "p".into(),
            extra_config: r#"{"model":null}"#.into(),
            ..Default::default()
        };
        assert!(build_request(&config(Provider::OpenaiCompatible), &prompt, "x").is_err());
        assert_eq!(
            endpoint(
                "https://example.com/custom/chat/completions/",
                Provider::OpenaiCompatible,
                "m"
            )
            .unwrap()
            .path(),
            "/custom/chat/completions"
        );
        assert!(endpoint("https://example.com?token=x", Provider::Google, "m").is_err());
    }

    #[test]
    fn provider_text_parsing_excludes_reasoning_and_empty_results() {
        assert_eq!(parse_text(Provider::Google, &json!({"candidates":[{"content":{"parts":[{"thought":true,"text":"private"},{"text":"output"}]}}]})).unwrap(), "output");
        assert_eq!(parse_text(Provider::OpenaiResponses, &json!({"output":[{"type":"message","content":[{"type":"output_text","text":"out"}]}]})).unwrap(), "out");
        assert_eq!(parse_text(Provider::Anthropic, &json!({"content":[{"type":"thinking","text":"private"},{"type":"text","text":"out"}]})).unwrap(), "out");
        assert!(
            parse_text(
                Provider::OpenaiCompatible,
                &json!({"choices":[{"message":{"content":" "}}]})
            )
            .is_err()
        );
    }

    // Include a final transport marker even after a failed finish reason: neither
    // [DONE] nor message_stop is evidence that earlier text was a complete result.
    fn completion_fixture(provider: Provider, reason: Value) -> (Value, String) {
        let (response, events) = match provider {
            Provider::Anthropic => (
                json!({"content":[{"type":"text","text":"rewritten"}],"stop_reason":reason}),
                vec![
                    json!({"type":"content_block_delta","delta":{"type":"text_delta","text":"rewritten"}}),
                    json!({"type":"message_delta","delta":{"stop_reason":reason}}),
                    json!({"type":"message_stop"}),
                ],
            ),
            Provider::Google => (
                json!({"candidates":[{"content":{"parts":[{"text":"rewritten"}]},"finishReason":reason}]}),
                vec![
                    json!({"candidates":[{"content":{"parts":[{"text":"rewritten"}]}}]}),
                    json!({"candidates":[{"finishReason":reason}]}),
                ],
            ),
            _ => (
                json!({"choices":[{"message":{"content":"rewritten"},"finish_reason":reason}]}),
                vec![
                    json!({"choices":[{"index":0,"delta":{"content":"rewritten"}}]}),
                    json!({"choices":[{"index":0,"delta":{},"finish_reason":reason}]}),
                ],
            ),
        };
        let stream = events
            .iter()
            .map(|event| format!("data: {event}\n\n"))
            .chain(std::iter::once("data: [DONE]\n\n".into()))
            .collect();
        (response, stream)
    }

    #[test]
    fn json_and_streams_accept_complete_and_compatible_finish_reasons() {
        for provider in Provider::ALL {
            if provider == Provider::OpenaiResponses {
                continue;
            }
            let complete = match provider {
                Provider::Anthropic => "end_turn",
                Provider::Google => "STOP",
                _ => "stop",
            };
            for reason in [
                json!(complete),
                Value::Null,
                json!(""),
                json!("vendor_complete"),
            ] {
                let (response, stream) = completion_fixture(provider, reason);
                assert_eq!(parse_text(provider, &response).unwrap(), "rewritten");
                assert_eq!(parse_stream(provider, &stream).unwrap(), "rewritten");
            }
        }
        let (response, stream) = completion_fixture(Provider::Anthropic, json!("stop_sequence"));
        assert_eq!(
            parse_text(Provider::Anthropic, &response).unwrap(),
            "rewritten"
        );
        assert_eq!(
            parse_stream(Provider::Anthropic, &stream).unwrap(),
            "rewritten"
        );
    }

    #[test]
    fn json_and_streams_reject_nonfinal_results_despite_transport_completion() {
        for (provider, reasons) in [
            (
                Provider::OpenaiCompatible,
                &["length", "content_filter", "tool_calls", "function_call"][..],
            ),
            (Provider::OpenaiCompletions, &["length"][..]),
            (
                Provider::Deepseek,
                &["length", "insufficient_system_resource", "aborted"][..],
            ),
            (Provider::Qwen, &["length"][..]),
            (Provider::Glm, &["length"][..]),
            (
                Provider::Anthropic,
                &[
                    "max_tokens",
                    "tool_use",
                    "pause_turn",
                    "refusal",
                    "model_context_window_exceeded",
                ][..],
            ),
            (
                Provider::Google,
                &[
                    "MAX_TOKENS",
                    "SAFETY",
                    "RECITATION",
                    "LANGUAGE",
                    "OTHER",
                    "BLOCKLIST",
                    "PROHIBITED_CONTENT",
                    "SPII",
                    "MALFORMED_FUNCTION_CALL",
                    "UNEXPECTED_TOOL_CALL",
                    "TOO_MANY_TOOL_CALLS",
                    "MISSING_THOUGHT_SIGNATURE",
                    "MALFORMED_RESPONSE",
                    "ESCALATION",
                    "PUP_LIMITED_DISABLED",
                    "IMAGE_SAFETY",
                    "IMAGE_PROHIBITED_CONTENT",
                    "IMAGE_OTHER",
                    "NO_IMAGE",
                    "IMAGE_RECITATION",
                ][..],
            ),
        ] {
            for reason in reasons {
                let (response, stream) = completion_fixture(provider, json!(reason));
                for result in [
                    parse_text(provider, &response),
                    parse_stream(provider, &stream),
                ] {
                    assert!(
                        matches!(result, Err(RewriteError::Response(ref message)) if message.contains(reason)),
                        "{provider:?} / {reason}: {result:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn unspecified_google_finish_reason_does_not_complete_a_stream() {
        let (_, stream) = completion_fixture(Provider::Google, json!("FINISH_REASON_UNSPECIFIED"));
        assert!(matches!(
            parse_stream(
                Provider::Google,
                stream.trim_end_matches("data: [DONE]\n\n")
            ),
            Err(RewriteError::Response(_))
        ));
    }

    #[test]
    fn incomplete_and_failed_streams_never_return_partial_text() {
        let partial = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"}}]}\n\n";
        assert!(parse_stream(Provider::OpenaiCompatible, partial).is_err());
        assert_eq!(
            parse_stream(
                Provider::OpenaiCompatible,
                &format!("{partial}data: [DONE]\n\n")
            )
            .unwrap(),
            "partial"
        );
        assert!(
            parse_stream(
                Provider::OpenaiCompatible,
                &format!("{partial}data: {{\"error\":{{\"code\":\"rate_limit_error\"}}}}\n\n")
            )
            .is_err()
        );
    }

    #[test]
    fn streams_support_provider_completion_and_ignore_reasoning() {
        for (provider, body, expected) in [
            (
                Provider::OpenaiResponses,
                "event: response.output_text.delta\r\ndata: {\"delta\":\"old\"}\r\n\r\nevent: response.completed\r\ndata: {\"response\":{\"output_text\":\"final\"}}\r\n\r\n",
                "final",
            ),
            (
                Provider::Anthropic,
                "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"private\"}}\n\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"out\"}}\n\ndata: {\"type\":\"message_stop\"}\n\n",
                "out",
            ),
            (
                Provider::Google,
                "data: {\"candidates\":[{\"content\":{\"parts\":[{\"thought\":true,\"text\":\"private\"},{\"text\":\"out\"}]},\"finishReason\":\"STOP\"}]}\n\n",
                "out",
            ),
            (
                Provider::OpenaiCompatible,
                "\u{feff}: comment\ndata: {\"choices\":\n data: ignored\ndata: [{\"index\":0,\"delta\":{\"content\":\"out\"},\"finish_reason\":\"stop\"},{\"index\":1,\"delta\":{\"content\":\"other\"}}]}\n\n",
                "out",
            ),
        ] {
            assert_eq!(parse_stream(provider, body).unwrap(), expected);
        }
        for code in [json!(429), json!("503"), json!("overloaded_error")] {
            assert!(matches!(
                parse_stream(
                    Provider::OpenaiCompatible,
                    &format!("data: {}\n\n", json!({"error":{"code":code}}))
                ),
                Err(RewriteError::Request {
                    retryable: true,
                    ..
                })
            ));
        }
        assert!(
            parse_text(
                Provider::OpenaiResponses,
                &json!({"status":"incomplete","output_text":"partial"})
            )
            .is_err()
        );
    }

    async fn mock_server(
        statuses: Vec<(u16, String, String)>,
    ) -> (
        String,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        tokio::task::JoinHandle<()>,
    ) {
        mock_provider_server(Provider::OpenaiCompatible, statuses).await
    }

    async fn mock_provider_server(
        provider: Provider,
        statuses: Vec<(u16, String, String)>,
    ) -> (
        String,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        tokio::task::JoinHandle<()>,
    ) {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                loop {
                    let mut buffer = [0; 4096];
                    let n = socket.read(&mut buffer).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    request.extend_from_slice(&buffer[..n]);
                    if let Some(end) = request.windows(4).position(|b| b == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                        let length = headers
                            .lines()
                            .find_map(|h| h.strip_prefix("content-length: "))
                            .unwrap()
                            .parse::<usize>()
                            .unwrap();
                        if request.len() >= end + 4 + length {
                            let body: Value = serde_json::from_slice(&request[end + 4..]).unwrap();
                            let (input, key_header) = match provider {
                                Provider::Anthropic => {
                                    (&body["messages"][0]["content"], "x-api-key: key")
                                }
                                Provider::Google => (
                                    &body["contents"][0]["parts"][0]["text"],
                                    "x-goog-api-key: key",
                                ),
                                Provider::OpenaiResponses => {
                                    (&body["input"][0]["content"], "authorization: bearer key")
                                }
                                _ => (&body["messages"][1]["content"], "authorization: bearer key"),
                            };
                            assert_eq!(input, "selected");
                            let endpoint =
                                endpoint("http://localhost", provider, "default").unwrap();
                            assert!(headers.starts_with(
                                &format!("post {} ", endpoint.path()).to_ascii_lowercase()
                            ));
                            assert!(headers.contains(key_header));
                            break;
                        }
                    }
                }
                let index = count.fetch_add(1, Ordering::SeqCst).min(statuses.len() - 1);
                let (status, content_type, body) = &statuses[index];
                let response = format!(
                    "HTTP/1.1 {status} Mock\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });
        (url, calls, server)
    }

    fn http_config(url: String) -> Config {
        Config {
            rewrite: RewriteConfig {
                base_url: url,
                ..config(Provider::OpenaiCompatible)
            },
            retry_base_delay: 0.0,
            max_retry: 3,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn retry_count_permanent_errors_and_single_attempt_probes() {
        use std::sync::atomic::Ordering;
        for (status, retry, expected_calls) in [
            (503, true, 3),
            (429, true, 3),
            (401, true, 1),
            (200, true, 1),
            (503, false, 1),
        ] {
            let (url, calls, server) = mock_server(vec![(
                status,
                "application/json".into(),
                "{\"invalid\":true}".into(),
            )])
            .await;
            let client = RewriteClient::new(http_config(url)).unwrap();
            let prompt = RewritePrompt {
                prompt: "rewrite".into(),
                ..Default::default()
            };
            let result = tokio::time::timeout(
                Duration::from_secs(3),
                client.execute(&prompt, "selected", &CancellationToken::new(), retry),
            )
            .await
            .unwrap();
            assert!(result.is_err());
            assert_eq!(calls.load(Ordering::SeqCst), expected_calls);
            server.abort();
        }
    }

    #[tokio::test]
    async fn retries_service_stream_errors_and_discards_partial_text() {
        use std::sync::atomic::Ordering;
        let partial = "data: {\"choices\":[{\"delta\":{\"content\":\"discard\"}}]}\n\ndata: {\"error\":{\"code\":\"rate_limit_error\"}}\n\n";
        let (url, calls, server) = mock_server(vec![
            (200, "text/event-stream".into(), partial.into()),
            (
                200,
                "application/json".into(),
                json!({"choices":[{"message":{"content":"final"}}]}).to_string(),
            ),
        ])
        .await;
        let client = RewriteClient::new(http_config(url)).unwrap();
        let prompt = RewritePrompt {
            prompt: "rewrite".into(),
            ..Default::default()
        };
        assert_eq!(
            client
                .execute(&prompt, "selected", &CancellationToken::new(), true)
                .await
                .unwrap(),
            "final"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        server.abort();
    }

    #[tokio::test]
    async fn incomplete_results_are_not_returned_or_retried() {
        use std::sync::atomic::Ordering;
        for provider in Provider::ALL {
            if provider == Provider::OpenaiResponses {
                continue;
            }
            let reason = match provider {
                Provider::Anthropic => "max_tokens",
                Provider::Google => "MAX_TOKENS",
                _ => "length",
            };
            let (response, stream) = completion_fixture(provider, json!(reason));
            for (content_type, body) in [
                ("application/json", response.to_string()),
                ("text/event-stream", stream),
            ] {
                let (url, calls, server) =
                    mock_provider_server(provider, vec![(200, content_type.into(), body)]).await;
                let mut config = http_config(url);
                config.rewrite.provider = provider;
                let client = RewriteClient::new(config).unwrap();
                let prompt = RewritePrompt {
                    prompt: "rewrite".into(),
                    ..Default::default()
                };
                let result = tokio::time::timeout(
                    Duration::from_secs(3),
                    client.execute(&prompt, "selected", &CancellationToken::new(), true),
                )
                .await
                .unwrap();
                server.abort();
                assert!(
                    matches!(result, Err(RewriteError::Response(_))),
                    "{provider:?} / {content_type}: {result:?}"
                );
                assert_eq!(calls.load(Ordering::SeqCst), 1);
            }
        }
    }

    #[tokio::test]
    async fn cancellation_interrupts_backoff_without_another_request() {
        use std::sync::atomic::Ordering;
        let (url, calls, server) =
            mock_server(vec![(503, "application/json".into(), "{}".into())]).await;
        let mut config = http_config(url);
        config.retry_base_delay = 60.0;
        let client = RewriteClient::new(config).unwrap();
        let token = CancellationToken::new();
        let task_token = token.clone();
        let task = tokio::spawn(async move {
            client
                .execute(
                    &RewritePrompt {
                        prompt: "rewrite".into(),
                        ..Default::default()
                    },
                    "selected",
                    &task_token,
                    true,
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(3), async {
            while calls.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        token.cancel();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), task)
                .await
                .unwrap()
                .unwrap(),
            Err(RewriteError::Canceled)
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn cancellation_interrupts_a_stalled_response_body() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = RewriteClient::new(http_config(format!(
            "http://{}",
            listener.local_addr().unwrap()
        )))
        .unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = [0; 4096];
            let _ = stream.read(&mut bytes).await.unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 99999\r\n\r\n{")
                .await
                .unwrap();
            tx.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        let token = CancellationToken::new();
        let task_token = token.clone();
        let task = tokio::spawn(async move {
            client
                .execute(
                    &RewritePrompt {
                        prompt: "rewrite".into(),
                        ..Default::default()
                    },
                    "selected",
                    &task_token,
                    true,
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(3), rx)
            .await
            .unwrap()
            .unwrap();
        token.cancel();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), task)
                .await
                .unwrap()
                .unwrap(),
            Err(RewriteError::Canceled)
        ));
        server.abort();
    }

    #[tokio::test]
    async fn oversized_responses_and_error_details_are_bounded() {
        for (body, expected) in [
            ("a".repeat(2 * 1024 * 1024 + 1), "2 MiB"),
            (
                json!({"error":{"message":"key","code":"invalid_request"}}).to_string(),
                "[redacted]",
            ),
        ] {
            let (url, _, server) = mock_server(vec![(200, "application/json".into(), body)]).await;
            let client = RewriteClient::new(http_config(url)).unwrap();
            let error = client
                .execute(
                    &RewritePrompt {
                        prompt: "rewrite".into(),
                        ..Default::default()
                    },
                    "selected",
                    &CancellationToken::new(),
                    true,
                )
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains(expected), "{error}");
            server.abort();
        }
    }
}
