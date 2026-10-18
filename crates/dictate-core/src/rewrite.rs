//! Provider wire formats and bounded, cancellable text requests.
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::{Config, additional_parameters};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Provider {
    #[default]
    #[serde(rename = "custom_openai_compatible")]
    OpenaiCompatible,
    #[serde(rename = "openai_responses")]
    OpenaiResponses,
    #[serde(rename = "openai_chat_completions")]
    OpenaiCompletions,
    #[serde(rename = "azure_openai_responses")]
    AzureOpenaiResponses,
    #[serde(rename = "azure_openai_chat_completions")]
    AzureOpenaiChatCompletions,
    #[serde(rename = "anthropic_claude")]
    Anthropic,
    #[serde(rename = "google_gemini")]
    Google,
    #[serde(rename = "google_vertex_ai")]
    VertexAi,
    #[serde(rename = "xai_grok")]
    Xai,
    #[serde(rename = "alibaba_qwen_text")]
    QwenText,
    #[serde(rename = "alibaba_qwen_multimodal")]
    QwenMultimodal,
    #[serde(rename = "moonshot_ai_kimi")]
    Kimi,
    #[serde(rename = "z_ai_glm")]
    Glm,
    #[serde(rename = "deepseek_deepseek")]
    Deepseek,
    #[serde(rename = "volcengine_ark_chat_completions")]
    VolcengineArkChatCompletions,
    #[serde(rename = "volcengine_ark_responses")]
    VolcengineArkResponses,
    #[serde(rename = "baidu_qianfan")]
    BaiduQianfan,
    #[serde(rename = "tencent_tokenhub")]
    TencentTokenHub,
    #[serde(rename = "minimax_chat_completion_v2")]
    MinimaxChatCompletionV2,
    #[serde(rename = "mistral_ai_mistral")]
    Mistral,
    #[serde(rename = "groq_cloud")]
    Groq,
    #[serde(rename = "cohere_compatibility_api")]
    Cohere,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WireFormat {
    OpenaiChat,
    OpenaiResponses,
    Gemini,
    Anthropic,
    DashscopeText,
    DashscopeMultimodal,
    MinimaxChatCompletionV2,
}

impl Provider {
    pub const ALL: [Self; 22] = [
        Self::OpenaiCompatible,
        Self::OpenaiResponses,
        Self::OpenaiCompletions,
        Self::AzureOpenaiResponses,
        Self::AzureOpenaiChatCompletions,
        Self::Anthropic,
        Self::Google,
        Self::VertexAi,
        Self::Xai,
        Self::QwenText,
        Self::QwenMultimodal,
        Self::Kimi,
        Self::Glm,
        Self::Deepseek,
        Self::VolcengineArkChatCompletions,
        Self::VolcengineArkResponses,
        Self::BaiduQianfan,
        Self::TencentTokenHub,
        Self::MinimaxChatCompletionV2,
        Self::Mistral,
        Self::Groq,
        Self::Cohere,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::OpenaiCompatible => "Custom / OpenAI-Compatible",
            Self::OpenaiResponses => "OpenAI / Responses",
            Self::OpenaiCompletions => "OpenAI / Chat Completions",
            Self::AzureOpenaiResponses => "Azure OpenAI / Responses",
            Self::AzureOpenaiChatCompletions => "Azure OpenAI / Chat Completions",
            Self::Anthropic => "Anthropic / Claude",
            Self::Google => "Google / Gemini",
            Self::VertexAi => "Google / Vertex AI",
            Self::Xai => "xAI / Grok",
            Self::QwenText => "Alibaba / Qwen Text",
            Self::QwenMultimodal => "Alibaba / Qwen Multimodal",
            Self::Kimi => "Moonshot AI / Kimi",
            Self::Glm => "Z.AI / GLM",
            Self::Deepseek => "DeepSeek / DeepSeek",
            Self::VolcengineArkChatCompletions => "Volcengine / Ark / Chat Completions",
            Self::VolcengineArkResponses => "Volcengine / Ark / Responses",
            Self::BaiduQianfan => "Baidu / Qianfan",
            Self::TencentTokenHub => "Tencent / TokenHub",
            Self::MinimaxChatCompletionV2 => "MiniMax / ChatCompletion V2",
            Self::Mistral => "Mistral AI / Mistral",
            Self::Groq => "Groq / Cloud",
            Self::Cohere => "Cohere / Compatibility API",
        }
    }

    const fn wire_format(self) -> WireFormat {
        match self {
            Self::OpenaiResponses | Self::AzureOpenaiResponses | Self::VolcengineArkResponses => {
                WireFormat::OpenaiResponses
            }
            Self::Google | Self::VertexAi => WireFormat::Gemini,
            Self::Anthropic => WireFormat::Anthropic,
            Self::QwenText => WireFormat::DashscopeText,
            Self::QwenMultimodal => WireFormat::DashscopeMultimodal,
            Self::MinimaxChatCompletionV2 => WireFormat::MinimaxChatCompletionV2,
            _ => WireFormat::OpenaiChat,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinPromptIcon {
    Chat,
    Messages,
    Translate,
    Eye,
    Feather,
    Document,
    Shield,
    PieChart,
    Tags,
    TextCard,
    Box,
    Flask,
    Globe,
    Cookie,
    Window,
    Plug,
    Settings,
    Layers,
    Key,
    Network,
    Hierarchy,
    BarChart,
    Microphone,
    Fingerprint,
    Layout,
}

impl BuiltinPromptIcon {
    pub const ALL: [Self; 25] = [
        Self::Chat,
        Self::Messages,
        Self::Translate,
        Self::Eye,
        Self::Feather,
        Self::Document,
        Self::Shield,
        Self::PieChart,
        Self::Tags,
        Self::TextCard,
        Self::Box,
        Self::Flask,
        Self::Globe,
        Self::Cookie,
        Self::Window,
        Self::Plug,
        Self::Settings,
        Self::Layers,
        Self::Key,
        Self::Network,
        Self::Hierarchy,
        Self::BarChart,
        Self::Microphone,
        Self::Fingerprint,
        Self::Layout,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Chat => "Chat",
            Self::Messages => "Messages",
            Self::Translate => "Translate",
            Self::Eye => "Eye",
            Self::Feather => "Feather",
            Self::Document => "Document",
            Self::Shield => "Shield",
            Self::PieChart => "Pie chart",
            Self::Tags => "Tags",
            Self::TextCard => "Text card",
            Self::Box => "Box",
            Self::Flask => "Flask",
            Self::Globe => "Globe",
            Self::Cookie => "Cookie",
            Self::Window => "Window",
            Self::Plug => "Plug",
            Self::Settings => "Settings",
            Self::Layers => "Layers",
            Self::Key => "Key",
            Self::Network => "Network",
            Self::Hierarchy => "Hierarchy",
            Self::BarChart => "Bar chart",
            Self::Microphone => "Microphone",
            Self::Fingerprint => "Fingerprint",
            Self::Layout => "Layout",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum PromptIcon {
    #[default]
    Default,
    Builtin(BuiltinPromptIcon),
    /// Relative reference in the application's managed icon directory.
    Custom(String),
}

impl<'de> Deserialize<'de> for PromptIcon {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "kind", content = "value", rename_all = "snake_case")]
        enum IconValue {
            Default,
            Builtin(BuiltinPromptIcon),
            Custom(String),
        }
        // A damaged or newer icon must not prevent otherwise valid prompts
        // from loading. Resource availability is checked by the GUI cache.
        let value = Value::deserialize(deserializer)?;
        Ok(match serde_json::from_value::<IconValue>(value) {
            Ok(IconValue::Builtin(icon)) => Self::Builtin(icon),
            Ok(IconValue::Custom(path)) if !path.trim().is_empty() => Self::Custom(path),
            _ => Self::Default,
        })
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
    pub icon: PromptIcon,
    pub provider: Option<Provider>,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

impl Default for RewritePrompt {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            title: String::new(),
            prompt: String::new(),
            extra_config: String::new(),
            hotkey: String::new(),
            icon: PromptIcon::default(),
            provider: None,
            base_url: String::new(),
            api_key: String::new(),
            model: String::new(),
        }
    }
}

impl RewritePrompt {
    /// A selected provider owns all API fields, including empty ones. Inherit
    /// the entire main API only when no prompt provider is selected.
    pub fn effective_api<'a>(&'a self, main: &'a RewriteConfig) -> RewriteApi<'a> {
        match self.provider {
            Some(provider) => RewriteApi {
                provider,
                base_url: &self.base_url,
                api_key: &self.api_key,
                model: &self.model,
            },
            None => RewriteApi {
                provider: main.provider,
                base_url: &main.base_url,
                api_key: &main.api_key,
                model: &main.model,
            },
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.id.is_empty() || self.title.trim().is_empty() || self.prompt.trim().is_empty() {
            return Err("Prompt ID, title and content must not be empty".into());
        }
        if !self.hotkey.trim().is_empty() {
            crate::hotkey::parse_hotkey(&self.hotkey)
                .map_err(|e| format!("{}: {e}", self.title))?;
        }
        additional_parameters::parse(&self.extra_config)
            .map_err(|e| format!("{}: {e}", self.title))?;
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub struct RewriteApi<'a> {
    pub provider: Provider,
    pub base_url: &'a str,
    pub api_key: &'a str,
    pub model: &'a str,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RewriteConfig {
    pub provider: Provider,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub menu_enabled: bool,
    pub menu_key: String,
    pub prompts: Vec<RewritePrompt>,
}

impl RewriteConfig {
    pub fn validate(&self) -> Result<(), String> {
        self.validate_prompts()?;
        if self.menu_enabled {
            crate::hotkey::parse_hotkey(&self.menu_key)
                .map_err(|error| format!("Menu key: {error}"))?;
        }
        Ok(())
    }

    /// Prompt editor commits may occur before the main settings draft has a
    /// menu key. Only final settings application requires that key.
    pub fn validate_prompts(&self) -> Result<(), String> {
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
    pub provider: Provider,
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
    let api = prompt.effective_api(config);
    if api.api_key.trim().is_empty() {
        return Err(Configuration("Rewrite API key is empty".into()));
    }
    if prompt.prompt.trim().is_empty() || input.is_empty() {
        return Err(Configuration(
            "Prompt and selected text must not be empty".into(),
        ));
    }
    let mut base = json!({"model":api.model.trim()});
    match api.provider.wire_format() {
        WireFormat::OpenaiResponses => {
            base["instructions"] = json!(prompt.prompt);
            base["input"] = json!([{"role":"user","content":input}]);
        }
        WireFormat::Gemini => {
            base["systemInstruction"] = json!({"parts":[{"text":prompt.prompt}]});
            base["contents"] = json!([{"role":"user","parts":[{"text":input}]}]);
        }
        WireFormat::Anthropic => {
            base["system"] = json!(prompt.prompt);
            base["max_tokens"] = json!(4096);
            base["messages"] = json!([{"role":"user","content":input}]);
        }
        WireFormat::OpenaiChat | WireFormat::MinimaxChatCompletionV2 => {
            base["messages"] =
                json!([{"role":"system","content":prompt.prompt},{"role":"user","content":input}])
        }
        WireFormat::DashscopeText => {
            base["input"] = json!({"messages":[
                {"role":"system","content":prompt.prompt},
                {"role":"user","content":input}
            ]});
            base["parameters"] = json!({"result_format":"message","incremental_output":true});
        }
        WireFormat::DashscopeMultimodal => {
            base["input"] = json!({"messages":[
                {"role":"system","content":prompt.prompt},
                {"role":"user","content":[{"text":input}]}
            ]});
            base["parameters"] = json!({"result_format":"message","incremental_output":true});
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
    let url = endpoint(api.base_url, api.provider, model)?;
    let headers = match api.provider {
        Provider::Anthropic => vec![
            ("x-api-key", api.api_key.to_owned()),
            ("anthropic-version", "2023-06-01".into()),
        ],
        Provider::Google => vec![("x-goog-api-key", api.api_key.to_owned())],
        Provider::AzureOpenaiResponses | Provider::AzureOpenaiChatCompletions => {
            vec![("api-key", api.api_key.to_owned())]
        }
        Provider::QwenText | Provider::QwenMultimodal => vec![
            ("Authorization", format!("Bearer {}", api.api_key)),
            ("X-DashScope-SSE", "enable".into()),
        ],
        _ => vec![("Authorization", format!("Bearer {}", api.api_key))],
    };
    if api.provider.wire_format() == WireFormat::Gemini {
        body.as_object_mut().unwrap().remove("model");
    }
    Ok(Request {
        provider: api.provider,
        url,
        headers,
        body,
    })
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
    if provider.wire_format() == WireFormat::Gemini {
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
        if provider == Provider::VertexAi {
            if !is_vertex_publisher_path(&path) {
                return Err(RewriteError::Configuration(
                    "Vertex AI Base URL must include /v1/projects/{project}/locations/{location}/publishers/google".into(),
                ));
            }
        } else if path.is_empty() {
            path = "/v1beta".into();
        }
        path.push_str(&format!("/models/{model}:generateContent"));
    } else {
        let suffix = match provider.wire_format() {
            WireFormat::OpenaiResponses => "/responses",
            WireFormat::Anthropic => "/messages",
            WireFormat::OpenaiChat => "/chat/completions",
            WireFormat::Gemini => unreachable!("Gemini endpoints are handled above"),
            WireFormat::DashscopeText => "/services/aigc/text-generation/generation",
            WireFormat::DashscopeMultimodal => "/services/aigc/multimodal-generation/generation",
            WireFormat::MinimaxChatCompletionV2 => "/text/chatcompletion_v2",
        };
        if path.is_empty() {
            path = match provider {
                Provider::Deepseek => "",
                Provider::QwenText | Provider::QwenMultimodal => "/api/v1",
                Provider::Glm => "/api/paas/v4",
                Provider::AzureOpenaiResponses | Provider::AzureOpenaiChatCompletions => {
                    "/openai/v1"
                }
                Provider::VolcengineArkChatCompletions | Provider::VolcengineArkResponses => {
                    "/api/v3"
                }
                Provider::BaiduQianfan => "/v2",
                Provider::Groq => "/openai/v1",
                Provider::Cohere => "/compatibility/v1",
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

fn is_vertex_publisher_path(path: &str) -> bool {
    let segments = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    if segments.len() < 7 {
        return false;
    }
    let tail = &segments[segments.len() - 7..];
    tail[0] == "v1"
        && tail[1] == "projects"
        && !tail[2].is_empty()
        && tail[3] == "locations"
        && !tail[4].is_empty()
        && tail[5] == "publishers"
        && tail[6] == "google"
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

fn dashscope_text(value: &Value) -> String {
    value.as_str().map(str::to_owned).unwrap_or_else(|| {
        value
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|block| block["text"].as_str())
            .collect()
    })
}

fn validate_finish_reason(provider: Provider, reason: &Value) -> Result<(), RewriteError> {
    let Some(reason) = reason.as_str() else {
        return Ok(());
    };
    // Some compatible services omit the reason or add their own values. Reject the
    // documented nonfinal outcomes without requiring every service to use "stop".
    let incomplete = match provider.wire_format() {
        WireFormat::Anthropic => matches!(
            reason,
            "max_tokens" | "tool_use" | "pause_turn" | "refusal" | "model_context_window_exceeded"
        ),
        WireFormat::Gemini => !matches!(reason, "STOP" | "FINISH_REASON_STOP"),
        WireFormat::OpenaiResponses => false,
        WireFormat::OpenaiChat | WireFormat::MinimaxChatCompletionV2 => matches!(
            reason,
            "length"
                | "content_filter"
                | "tool_calls"
                | "function_call"
                | "insufficient_system_resource"
                | "aborted"
        ),
        // DashScope sends the literal string "null" in intermediate SSE
        // packets. It is not a terminal result, but it is a valid streaming
        // state while waiting for the final `stop` packet.
        WireFormat::DashscopeText | WireFormat::DashscopeMultimodal => {
            !matches!(reason, "stop" | "null")
        }
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

fn validate_terminal_finish_reason(provider: Provider, reason: &Value) -> Result<(), RewriteError> {
    if matches!(
        provider.wire_format(),
        WireFormat::Gemini | WireFormat::DashscopeText | WireFormat::DashscopeMultimodal
    ) && !reason.as_str().is_some_and(|reason| !reason.is_empty())
    {
        return Err(RewriteError::Response(format!(
            "Rewrite response is not complete ({}: missing finishReason)",
            provider.label()
        )));
    }
    if matches!(
        provider.wire_format(),
        WireFormat::DashscopeText | WireFormat::DashscopeMultimodal
    ) && reason.as_str() == Some("null")
    {
        return Err(RewriteError::Response(format!(
            "Rewrite response is not complete ({}: null)",
            provider.label()
        )));
    }
    validate_finish_reason(provider, reason)
}

fn validate_response_status(provider: Provider, value: &Value) -> Result<(), RewriteError> {
    if provider.wire_format() == WireFormat::OpenaiResponses
        && matches!(
            value["status"].as_str(),
            Some("failed" | "incomplete" | "cancelled" | "in_progress" | "queued")
        )
    {
        return Err(RewriteError::Response(
            "Rewrite response is not complete".into(),
        ));
    }
    Ok(())
}

pub fn parse_text(provider: Provider, value: &Value) -> Result<String, RewriteError> {
    if value.get("error").is_some_and(|error| !error.is_null()) || value["type"] == "error" {
        return Err(stream_error(value));
    }
    validate_response_status(provider, value)?;
    match provider.wire_format() {
        WireFormat::Anthropic => validate_finish_reason(provider, &value["stop_reason"])?,
        WireFormat::Gemini => {
            validate_terminal_finish_reason(provider, &value["candidates"][0]["finishReason"])?
        }
        WireFormat::OpenaiResponses => {}
        WireFormat::OpenaiChat | WireFormat::MinimaxChatCompletionV2 => {
            validate_finish_reason(provider, &value["choices"][0]["finish_reason"])?
        }
        WireFormat::DashscopeText | WireFormat::DashscopeMultimodal => {
            validate_terminal_finish_reason(
                provider,
                &value["output"]["choices"][0]["finish_reason"],
            )?
        }
    }
    let text = match provider.wire_format() {
        WireFormat::OpenaiResponses => value["output_text"]
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
        WireFormat::Anthropic => blocks(&value["content"], "text"),
        WireFormat::Gemini => value["candidates"][0]["content"]["parts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|v| v["thought"] != true)
            .filter_map(|v| v["text"].as_str())
            .collect(),
        WireFormat::OpenaiChat | WireFormat::MinimaxChatCompletionV2 => {
            let content = &value["choices"][0]["message"]["content"];
            content
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| blocks(content, "text"))
        }
        WireFormat::DashscopeText | WireFormat::DashscopeMultimodal => {
            dashscope_text(&value["output"]["choices"][0]["message"]["content"])
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
                if !matches!(
                    provider.wire_format(),
                    WireFormat::Gemini
                        | WireFormat::DashscopeText
                        | WireFormat::DashscopeMultimodal
                ) {
                    completed = true;
                }
                name.clear();
                continue;
            }
            let root: Value = serde_json::from_str(&payload)
                .map_err(|e| RewriteError::Response(format!("Invalid rewrite stream: {e}")))?;
            let kind = root["type"].as_str().unwrap_or(&name);
            if root.get("error").is_some()
                || matches!(kind, "error" | "response.failed")
                || (matches!(
                    provider.wire_format(),
                    WireFormat::DashscopeText | WireFormat::DashscopeMultimodal
                ) && root.get("code").is_some()
                    && root.get("message").is_some())
            {
                return Err(stream_error(&root));
            }
            if kind == "response.incomplete" {
                return Err(RewriteError::Response("Incomplete rewrite stream".into()));
            }
            match provider.wire_format() {
                WireFormat::OpenaiResponses => match kind {
                    "response.output_text.delta" => {
                        text.push_str(root["delta"].as_str().unwrap_or(""))
                    }
                    "response.completed" => {
                        let result = &root["response"];
                        validate_response_status(provider, result)?;
                        completed = true;
                        if result.get("output").is_some() || result.get("output_text").is_some() {
                            final_text = Some(parse_text(provider, result)?);
                        }
                    }
                    _ => {}
                },
                WireFormat::Anthropic => match kind {
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
                WireFormat::Gemini => {
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
                    if matches!(
                        candidate["finishReason"].as_str(),
                        Some("STOP" | "FINISH_REASON_STOP")
                    ) {
                        completed = true;
                    }
                }
                WireFormat::OpenaiChat | WireFormat::MinimaxChatCompletionV2 => {
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
                WireFormat::DashscopeText | WireFormat::DashscopeMultimodal => {
                    let choice = &root["output"]["choices"][0];
                    if !choice.is_null() {
                        validate_finish_reason(provider, &choice["finish_reason"])?;
                        text.push_str(&dashscope_text(&choice["message"]["content"]));
                        if choice["finish_reason"] == "stop" {
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
        self.test_prompt_connection(&RewritePrompt::default(), cancel)
            .await
    }

    /// Test only the selected API. Draft prompt contents, title, hotkey and extra
    /// parameters are deliberately excluded from this single-request probe.
    pub async fn test_prompt_connection(
        &self,
        prompt: &RewritePrompt,
        cancel: &CancellationToken,
    ) -> Result<(), RewriteError> {
        let probe = RewritePrompt {
            prompt: "Reply briefly with OK.".into(),
            provider: prompt.provider,
            base_url: prompt.base_url.clone(),
            api_key: prompt.api_key.clone(),
            model: prompt.model.clone(),
            ..Default::default()
        };
        self.debug(
            &probe,
            format_args!("[upload] rewrite event=connectivity-start"),
        );
        let started = std::time::Instant::now();
        let result = self
            .execute(&probe, "Connection test.", cancel, false)
            .await
            .map(|_| ());
        match &result {
            Ok(()) => self.debug(
                &probe,
                format_args!(
                    "[upload] rewrite event=connectivity-complete elapsed_ms={}",
                    started.elapsed().as_millis()
                ),
            ),
            Err(error) => self.debug(
                &probe,
                format_args!(
                    "[upload] rewrite event=connectivity-failed kind={} elapsed_ms={}",
                    rewrite_error_kind(error),
                    started.elapsed().as_millis()
                ),
            ),
        }
        result
    }

    pub async fn execute(
        &self,
        prompt: &RewritePrompt,
        input: &str,
        cancel: &CancellationToken,
        retry: bool,
    ) -> Result<String, RewriteError> {
        if cancel.is_cancelled() {
            self.debug(
                prompt,
                format_args!("[upload] rewrite event=canceled phase=before-request"),
            );
            return Err(RewriteError::Canceled);
        }
        let request = match build_request(&self.config.rewrite, prompt, input) {
            Ok(request) => request,
            Err(error) => {
                let error = self.redact(error, prompt, input);
                self.debug(
                    prompt,
                    format_args!("[upload] rewrite event=failed kind=configuration"),
                );
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
                self.debug(
                    prompt,
                    format_args!("[upload] rewrite event=canceled phase=before-request"),
                );
                return Err(RewriteError::Canceled);
            }
            let started = std::time::Instant::now();
            self.debug(
                prompt,
                format_args!(
                    "[upload] rewrite event=request-start provider={} attempt={attempt} total={attempts} method=POST endpoint={} request_bytes={}",
                    request.provider.label(),
                    crate::debug_log::safe_url(request.url.as_str()),
                    request.body.to_string().len(),
                ),
            );
            let result = self
                .once(&request, prompt, cancel)
                .await
                .map_err(|error| self.redact(error, prompt, input));
            match &result {
                Ok(text) => self.debug(
                    prompt,
                    format_args!(
                        "[upload] rewrite event=completed characters={} elapsed_ms={}",
                        text.chars().count(),
                        started.elapsed().as_millis()
                    ),
                ),
                Err(error) => self.debug(
                    prompt,
                    format_args!(
                        "[upload] rewrite event=attempt-failed attempt={attempt} kind={} elapsed_ms={}",
                        rewrite_error_kind(error),
                        started.elapsed().as_millis(),
                    ),
                ),
            }
            match result {
                Err(RewriteError::Request {
                    retryable: true, ..
                }) if attempt < attempts => {
                    self.debug(
                        prompt,
                        format_args!(
                            "[upload] rewrite event=retry delay_ms={}",
                            Duration::from_secs_f64(delay.min(86400.0)).as_millis()
                        ),
                    );
                    tokio::select! {
                        _ = cancel.cancelled() => {
                            self.debug(prompt, format_args!("[upload] rewrite event=canceled phase=retry-wait"));
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
        prompt: &RewritePrompt,
        cancel: &CancellationToken,
    ) -> Result<String, RewriteError> {
        let operation = async {
            let started = std::time::Instant::now();
            let mut builder = self
                .client
                .post(request.url.clone())
                .header("Content-Type", "application/json")
                .header("User-Agent", "dictate-client/1.0")
                .body(request.body.to_string());
            for (key, value) in &request.headers {
                builder = builder.header(*key, value);
            }
            let mut response = match builder.send().await {
                Ok(response) => response,
                Err(error) => {
                    self.debug(
                        prompt,
                        format_args!(
                            "[upload] rewrite event=request-failed kind=network elapsed_ms={}",
                            started.elapsed().as_millis()
                        ),
                    );
                    return Err(self.network_error(error));
                }
            };
            let status = response.status();
            let stream = response
                .headers()
                .get("content-type")
                .and_then(|h| h.to_str().ok())
                .is_some_and(|h| h.to_ascii_lowercase().contains("text/event-stream"));
            self.debug(
                prompt,
                format_args!(
                    "[upload] rewrite event=response-headers status={} stream={} elapsed_ms={}",
                    status.as_u16(),
                    stream,
                    started.elapsed().as_millis()
                ),
            );
            let mut bytes = Vec::new();
            let mut chunks = 0usize;
            while let Some(chunk) = match response.chunk().await {
                Ok(chunk) => chunk,
                Err(error) => {
                    self.debug(
                        prompt,
                        format_args!(
                            "[upload] rewrite event=response-read-failed kind=network elapsed_ms={}",
                            started.elapsed().as_millis()
                        ),
                    );
                    return Err(self.network_error(error));
                }
            } {
                if bytes.len() + chunk.len() > 2 * 1024 * 1024 {
                    self.debug(
                        prompt,
                        format_args!("[upload] rewrite event=response-read-failed kind=too-large"),
                    );
                    return Err(RewriteError::Response(
                        "Rewrite response exceeds 2 MiB".into(),
                    ));
                }
                bytes.extend_from_slice(&chunk);
                chunks += 1;
            }
            self.debug(
                prompt,
                format_args!(
                    "[upload] rewrite event=response-complete status={} bytes={} chunks={} elapsed_ms={}",
                    status.as_u16(),
                    bytes.len(),
                    chunks,
                    started.elapsed().as_millis()
                ),
            );
            let body = match String::from_utf8(bytes) {
                Ok(body) => body,
                Err(_) => {
                    self.debug(
                        prompt,
                        format_args!("[upload] rewrite event=parse-failed kind=encoding"),
                    );
                    return Err(RewriteError::Response(
                        "Rewrite response is not UTF-8".into(),
                    ));
                }
            };
            if !status.is_success() {
                self.debug(
                    prompt,
                    format_args!(
                        "[upload] rewrite event=response-rejected status={} response_bytes={}",
                        status.as_u16(),
                        body.len()
                    ),
                );
                return Err(RewriteError::Request {
                    message: format!("HTTP {status}: {body}"),
                    retryable: matches!(status.as_u16(), 408 | 429 | 500..=599),
                });
            }
            let result = if stream {
                parse_stream(request.provider, &body)
            } else {
                match serde_json::from_str(&body) {
                    Ok(response) => parse_text(request.provider, &response),
                    Err(error) => Err(RewriteError::Response(format!(
                        "Invalid rewrite JSON: {error}"
                    ))),
                }
            };
            match &result {
                Ok(text) => self.debug(
                    prompt,
                    format_args!(
                        "[upload] rewrite event=parse-complete characters={}",
                        text.chars().count()
                    ),
                ),
                Err(error) => self.debug(
                    prompt,
                    format_args!(
                        "[upload] rewrite event=parse-failed kind={}",
                        rewrite_error_kind(error)
                    ),
                ),
            }
            result
        };
        tokio::select! { biased;
            _ = cancel.cancelled() => Err(RewriteError::Canceled),
            result = operation => result,
        }
    }

    fn redact(&self, error: RewriteError, prompt: &RewritePrompt, input: &str) -> RewriteError {
        let clean = |message: String| {
            crate::debug_log::request_message_for_prompt_and_input(
                &message,
                &self.config,
                prompt,
                input,
            )
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
            RewriteError::Configuration(message) => RewriteError::Configuration(clean(message)),
            error => error,
        }
    }

    fn debug(&self, prompt: &RewritePrompt, message: std::fmt::Arguments<'_>) {
        if self.config.upload_debug {
            let message = crate::debug_log::request_message_for_prompt(
                &message.to_string(),
                &self.config,
                prompt,
            );
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

fn rewrite_error_kind(error: &RewriteError) -> &'static str {
    match error {
        RewriteError::Canceled => "canceled",
        RewriteError::Configuration(_) => "configuration",
        RewriteError::Response(_) => "response",
        RewriteError::Request { message, .. } if message.starts_with("HTTP ") => "http",
        RewriteError::Request { .. } => "network",
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
            ..Default::default()
        }
    }

    #[test]
    fn provider_catalog_uses_confirmed_ids_labels_and_order() {
        let expected = [
            (
                Provider::OpenaiCompatible,
                "custom_openai_compatible",
                "Custom / OpenAI-Compatible",
            ),
            (
                Provider::OpenaiResponses,
                "openai_responses",
                "OpenAI / Responses",
            ),
            (
                Provider::OpenaiCompletions,
                "openai_chat_completions",
                "OpenAI / Chat Completions",
            ),
            (
                Provider::AzureOpenaiResponses,
                "azure_openai_responses",
                "Azure OpenAI / Responses",
            ),
            (
                Provider::AzureOpenaiChatCompletions,
                "azure_openai_chat_completions",
                "Azure OpenAI / Chat Completions",
            ),
            (
                Provider::Anthropic,
                "anthropic_claude",
                "Anthropic / Claude",
            ),
            (Provider::Google, "google_gemini", "Google / Gemini"),
            (Provider::VertexAi, "google_vertex_ai", "Google / Vertex AI"),
            (Provider::Xai, "xai_grok", "xAI / Grok"),
            (
                Provider::QwenText,
                "alibaba_qwen_text",
                "Alibaba / Qwen Text",
            ),
            (
                Provider::QwenMultimodal,
                "alibaba_qwen_multimodal",
                "Alibaba / Qwen Multimodal",
            ),
            (Provider::Kimi, "moonshot_ai_kimi", "Moonshot AI / Kimi"),
            (Provider::Glm, "z_ai_glm", "Z.AI / GLM"),
            (
                Provider::Deepseek,
                "deepseek_deepseek",
                "DeepSeek / DeepSeek",
            ),
            (
                Provider::VolcengineArkChatCompletions,
                "volcengine_ark_chat_completions",
                "Volcengine / Ark / Chat Completions",
            ),
            (
                Provider::VolcengineArkResponses,
                "volcengine_ark_responses",
                "Volcengine / Ark / Responses",
            ),
            (Provider::BaiduQianfan, "baidu_qianfan", "Baidu / Qianfan"),
            (
                Provider::TencentTokenHub,
                "tencent_tokenhub",
                "Tencent / TokenHub",
            ),
            (
                Provider::MinimaxChatCompletionV2,
                "minimax_chat_completion_v2",
                "MiniMax / ChatCompletion V2",
            ),
            (
                Provider::Mistral,
                "mistral_ai_mistral",
                "Mistral AI / Mistral",
            ),
            (Provider::Groq, "groq_cloud", "Groq / Cloud"),
            (
                Provider::Cohere,
                "cohere_compatibility_api",
                "Cohere / Compatibility API",
            ),
        ];
        assert_eq!(Provider::ALL.len(), expected.len());
        for (index, (provider, id, label)) in expected.into_iter().enumerate() {
            assert_eq!(Provider::ALL[index], provider);
            assert_eq!(provider.label(), label);
            assert_eq!(
                serde_json::to_value(provider).unwrap(),
                Value::String(id.into())
            );
            assert_eq!(
                serde_json::from_value::<Provider>(Value::String(id.into())).unwrap(),
                provider
            );
        }
        assert!(
            serde_json::from_value::<Provider>(Value::String("openai_compatible".into())).is_err()
        );
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
                Provider::QwenText,
                "/api/v1/services/aigc/text-generation/generation",
                "Authorization",
            ),
            (
                Provider::QwenMultimodal,
                "/api/v1/services/aigc/multimodal-generation/generation",
                "Authorization",
            ),
            (
                Provider::Glm,
                "/api/paas/v4/chat/completions",
                "Authorization",
            ),
            (
                Provider::AzureOpenaiResponses,
                "/openai/v1/responses",
                "api-key",
            ),
            (
                Provider::AzureOpenaiChatCompletions,
                "/openai/v1/chat/completions",
                "api-key",
            ),
            (
                Provider::VertexAi,
                "/v1/projects/project/locations/global/publishers/google/models/override:generateContent",
                "Authorization",
            ),
            (Provider::Kimi, "/v1/chat/completions", "Authorization"),
            (
                Provider::VolcengineArkChatCompletions,
                "/api/v3/chat/completions",
                "Authorization",
            ),
            (
                Provider::VolcengineArkResponses,
                "/api/v3/responses",
                "Authorization",
            ),
            (
                Provider::BaiduQianfan,
                "/v2/chat/completions",
                "Authorization",
            ),
            (
                Provider::TencentTokenHub,
                "/v1/chat/completions",
                "Authorization",
            ),
            (
                Provider::MinimaxChatCompletionV2,
                "/v1/text/chatcompletion_v2",
                "Authorization",
            ),
            (Provider::Mistral, "/v1/chat/completions", "Authorization"),
            (Provider::Xai, "/v1/chat/completions", "Authorization"),
            (
                Provider::Groq,
                "/openai/v1/chat/completions",
                "Authorization",
            ),
            (
                Provider::Cohere,
                "/compatibility/v1/chat/completions",
                "Authorization",
            ),
        ] {
            let prompt = RewritePrompt {
                prompt: "Translate".into(),
                extra_config: r#"{"model":"override","metadata":{"source":"test"}}"#.into(),
                ..Default::default()
            };
            let mut api = config(provider);
            if provider == Provider::VertexAi {
                api.base_url =
                    "https://example.com/v1/projects/project/locations/global/publishers/google"
                        .into();
            }
            let request = build_request(&api, &prompt, "hello").unwrap();
            assert_eq!(request.url.path(), path);
            assert_eq!(request.headers[0].0, header);
            if provider.wire_format() == WireFormat::Gemini {
                assert!(request.body.get("model").is_none());
            } else {
                assert_eq!(request.body["model"], "override");
            }
            assert_eq!(request.body["metadata"]["source"], "test");
            assert!(request.body.to_string().contains("hello"));
            assert!(request.body.to_string().contains("Translate"));
        }
    }

    #[test]
    fn prompt_api_inheritance_ignores_hidden_fields_but_explicit_provider_overrides_all() {
        let main = config(Provider::OpenaiCompatible);
        let mut prompt = RewritePrompt {
            prompt: "rewrite".into(),
            base_url: "not a URL".into(),
            api_key: "hidden-key".into(),
            model: "hidden-model".into(),
            ..Default::default()
        };
        let inherited = build_request(&main, &prompt, "selected").unwrap();
        assert_eq!(inherited.provider, main.provider);
        assert_eq!(inherited.url.host_str(), Some("example.com"));
        assert_eq!(inherited.headers[0].1, "Bearer key");
        assert_eq!(inherited.body["model"], "default");

        // Selecting even the same provider makes all API fields independent.
        prompt.provider = Some(main.provider);
        assert!(matches!(
            build_request(&main, &prompt, "selected"),
            Err(RewriteError::Configuration(_))
        ));
        prompt.base_url = "https://independent.example/v2".into();
        let independent = build_request(&main, &prompt, "selected").unwrap();
        assert_eq!(
            independent.url.as_str(),
            "https://independent.example/v2/chat/completions"
        );
        assert_eq!(independent.headers[0].1, "Bearer hidden-key");
        assert_eq!(independent.body["model"], "hidden-model");
        for field in ["base_url", "api_key", "model"] {
            let mut incomplete = prompt.clone();
            match field {
                "base_url" => incomplete.base_url.clear(),
                "api_key" => incomplete.api_key.clear(),
                _ => incomplete.model.clear(),
            }
            assert!(
                matches!(
                    build_request(&main, &incomplete, "selected"),
                    Err(RewriteError::Configuration(_))
                ),
                "empty {field} must not fall back to the main API"
            );
        }
        prompt.model.clear();
        prompt.extra_config = r#"{"model":"from-extra","nested":{"values":[1,true,null]}}"#.into();
        let overridden = build_request(&main, &prompt, "selected").unwrap();
        assert_eq!(overridden.body["model"], "from-extra");
        assert_eq!(overridden.body["nested"]["values"], json!([1, true, null]));
    }

    #[test]
    fn legacy_prompt_api_defaults_and_configuration_roundtrip() {
        let legacy: RewritePrompt = serde_json::from_value(json!({
            "id":"stable-prompt", "title":"Polish", "prompt":"Polish the text",
            "hotkey":"ctrl+alt+w", "extra_config":"{}"
        }))
        .unwrap();
        assert_eq!(legacy.provider, None);
        assert!(legacy.base_url.is_empty() && legacy.api_key.is_empty() && legacy.model.is_empty());
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        for provider in [
            None,
            Some(Provider::Anthropic),
            Some(Provider::OpenaiCompatible),
        ] {
            let mut config = Config::default();
            config.rewrite.prompts.push(RewritePrompt {
                provider,
                base_url: "https://independent.example".into(),
                api_key: "independent-key".into(),
                model: "independent-model".into(),
                ..legacy.clone()
            });
            config.validate().unwrap();
            config.save(&path).unwrap();
            let restored = Config::load(&path).unwrap();
            assert_eq!(restored, config);
            assert_eq!(restored.rewrite.prompts[0].id, "stable-prompt");
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
        assert!(endpoint("https://example.com", Provider::VertexAi, "m").is_err());
    }

    #[test]
    fn provider_text_parsing_excludes_reasoning_and_empty_results() {
        assert_eq!(parse_text(Provider::Google, &json!({"candidates":[{"content":{"parts":[{"thought":true,"text":"private"},{"text":"output"}]},"finishReason":"STOP"}]})).unwrap(), "output");
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
        let (response, events) = match provider.wire_format() {
            WireFormat::OpenaiResponses => (
                json!({"status":"completed","output_text":"rewritten"}),
                vec![
                    json!({"type":"response.output_text.delta","delta":"rewritten"}),
                    json!({"type":"response.completed","response":{"status":"completed","output_text":"rewritten"}}),
                ],
            ),
            WireFormat::Anthropic => (
                json!({"content":[{"type":"text","text":"rewritten"}],"stop_reason":reason}),
                vec![
                    json!({"type":"content_block_delta","delta":{"type":"text_delta","text":"rewritten"}}),
                    json!({"type":"message_delta","delta":{"stop_reason":reason}}),
                    json!({"type":"message_stop"}),
                ],
            ),
            WireFormat::Gemini => (
                json!({"candidates":[{"content":{"parts":[{"text":"rewritten"}]},"finishReason":reason}]}),
                vec![
                    json!({"candidates":[{"content":{"parts":[{"text":"rewritten"}]}}]}),
                    json!({"candidates":[{"finishReason":reason}]}),
                ],
            ),
            WireFormat::OpenaiChat | WireFormat::MinimaxChatCompletionV2 => (
                json!({"choices":[{"message":{"content":"rewritten"},"finish_reason":reason}]}),
                vec![
                    json!({"choices":[{"index":0,"delta":{"content":"rewritten"}}]}),
                    json!({"choices":[{"index":0,"delta":{},"finish_reason":reason}]}),
                ],
            ),
            WireFormat::DashscopeText | WireFormat::DashscopeMultimodal => (
                json!({"output":{"choices":[{"message":{"content":"rewritten"},"finish_reason":reason}]}}),
                vec![
                    json!({"output":{"choices":[{"message":{"content":"rewritten"}}]}}),
                    json!({"output":{"choices":[{"finish_reason":reason}]}}),
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
            if provider.wire_format() == WireFormat::OpenaiResponses {
                continue;
            }
            let complete = match provider.wire_format() {
                WireFormat::Anthropic => "end_turn",
                WireFormat::Gemini => "STOP",
                WireFormat::OpenaiChat | WireFormat::MinimaxChatCompletionV2 => "stop",
                WireFormat::DashscopeText | WireFormat::DashscopeMultimodal => "stop",
                WireFormat::OpenaiResponses => unreachable!("skipped above"),
            };
            let reasons = if provider.wire_format() == WireFormat::Gemini {
                vec![json!(complete), json!("FINISH_REASON_STOP")]
            } else if matches!(
                provider.wire_format(),
                WireFormat::DashscopeText | WireFormat::DashscopeMultimodal
            ) {
                vec![json!(complete)]
            } else {
                vec![
                    json!(complete),
                    Value::Null,
                    json!(""),
                    json!("vendor_complete"),
                ]
            };
            for reason in reasons {
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
            (Provider::Kimi, &["length"][..]),
            (Provider::VolcengineArkChatCompletions, &["length"][..]),
            (Provider::BaiduQianfan, &["length"][..]),
            (Provider::TencentTokenHub, &["length"][..]),
            (Provider::MinimaxChatCompletionV2, &["length"][..]),
            (Provider::Mistral, &["length"][..]),
            (Provider::Xai, &["length"][..]),
            (Provider::Groq, &["length"][..]),
            (Provider::Cohere, &["length"][..]),
            (
                Provider::Deepseek,
                &["length", "insufficient_system_resource", "aborted"][..],
            ),
            (Provider::QwenText, &["length"][..]),
            (Provider::QwenMultimodal, &["length"][..]),
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
                    "CONTINUATION",
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
                    "FINISH_REASON_UNSPECIFIED",
                    "vendor_complete",
                ][..],
            ),
            (
                Provider::VertexAi,
                &[
                    "MAX_TOKENS",
                    "CONTINUATION",
                    "FINISH_REASON_UNSPECIFIED",
                    "vendor_complete",
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
    fn gemini_results_require_an_explicit_stop_reason() {
        for provider in [Provider::Google, Provider::VertexAi] {
            for reason in [Value::Null, json!("")] {
                let (response, _) = completion_fixture(provider, reason);
                assert!(parse_text(provider, &response).is_err(), "{provider:?}");
            }
            for reason in [Value::Null, json!("FINISH_REASON_UNSPECIFIED")] {
                let (_, stream) = completion_fixture(provider, reason);
                assert!(parse_stream(provider, &stream).is_err(), "{provider:?}");
            }
        }
    }

    #[test]
    fn dashscope_streams_allow_null_intermediate_reason_but_require_terminal_stop() {
        for provider in [Provider::QwenText, Provider::QwenMultimodal] {
            let stream = concat!(
                "data: {\"output\":{\"choices\":[{\"message\":{\"content\":\"answer\"},\"finish_reason\":\"null\"}]}}\n\n",
                "data: {\"output\":{\"choices\":[{\"finish_reason\":\"stop\"}]}}\n\n",
                "data: [DONE]\n\n",
            );
            assert_eq!(parse_stream(provider, stream).unwrap(), "answer");

            let response = json!({
                "output": {"choices": [{
                    "message": {"content": "partial"},
                    "finish_reason": "null"
                }]}
            });
            assert!(parse_text(provider, &response).is_err(), "{provider:?}");
        }
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
        let responses_partial = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"incomplete\"}}\n\n";
        assert!(parse_stream(Provider::OpenaiResponses, responses_partial).is_err());
    }

    #[test]
    fn streams_support_provider_completion_and_ignore_reasoning() {
        for (provider, body, expected) in [
            (
                Provider::OpenaiResponses,
                "event: response.output_text.delta\r\ndata: {\"delta\":\"old\"}\r\n\r\nevent: response.completed\r\ndata: {\"response\":{\"status\":\"completed\",\"output_text\":\"final\"}}\r\n\r\n",
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
        mock_api_server(provider, "key", "default", "selected", None, statuses).await
    }

    async fn mock_api_server(
        provider: Provider,
        api_key: &'static str,
        model: &'static str,
        expected_input: &'static str,
        expected_prompt: Option<&'static str>,
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
        let base_url = if provider == Provider::VertexAi {
            format!("{url}/v1/projects/project/locations/global/publishers/google")
        } else {
            url.clone()
        };
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
                            let (input, instruction, key_header) = match provider.wire_format() {
                                WireFormat::Anthropic => (
                                    &body["messages"][0]["content"],
                                    &body["system"],
                                    format!("x-api-key: {api_key}"),
                                ),
                                WireFormat::Gemini => (
                                    &body["contents"][0]["parts"][0]["text"],
                                    &body["systemInstruction"]["parts"][0]["text"],
                                    if provider == Provider::Google {
                                        format!("x-goog-api-key: {api_key}")
                                    } else {
                                        format!("authorization: bearer {api_key}")
                                    },
                                ),
                                WireFormat::OpenaiResponses => (
                                    &body["input"][0]["content"],
                                    &body["instructions"],
                                    if matches!(
                                        provider,
                                        Provider::AzureOpenaiResponses
                                            | Provider::AzureOpenaiChatCompletions
                                    ) {
                                        format!("api-key: {api_key}")
                                    } else {
                                        format!("authorization: bearer {api_key}")
                                    },
                                ),
                                WireFormat::OpenaiChat | WireFormat::MinimaxChatCompletionV2 => (
                                    &body["messages"][1]["content"],
                                    &body["messages"][0]["content"],
                                    if provider == Provider::AzureOpenaiChatCompletions {
                                        format!("api-key: {api_key}")
                                    } else {
                                        format!("authorization: bearer {api_key}")
                                    },
                                ),
                                WireFormat::DashscopeText => (
                                    &body["input"]["messages"][1]["content"],
                                    &body["input"]["messages"][0]["content"],
                                    format!("authorization: bearer {api_key}"),
                                ),
                                WireFormat::DashscopeMultimodal => (
                                    &body["input"]["messages"][1]["content"][0]["text"],
                                    &body["input"]["messages"][0]["content"],
                                    format!("authorization: bearer {api_key}"),
                                ),
                            };
                            assert_eq!(input, expected_input);
                            if let Some(expected_prompt) = expected_prompt {
                                assert_eq!(instruction, expected_prompt);
                                assert!(body.get("draft-only").is_none());
                            }
                            if provider.wire_format() != WireFormat::Gemini {
                                assert_eq!(body["model"], model);
                            }
                            let endpoint_base = if provider == Provider::VertexAi {
                                "http://localhost/v1/projects/project/locations/global/publishers/google"
                            } else {
                                "http://localhost"
                            };
                            let endpoint = endpoint(endpoint_base, provider, model).unwrap();
                            assert!(headers.starts_with(
                                &format!("post {} ", endpoint.path()).to_ascii_lowercase()
                            ));
                            assert!(headers.contains(&key_header));
                            if matches!(provider, Provider::QwenText | Provider::QwenMultimodal) {
                                assert!(headers.contains("x-dashscope-sse: enable"));
                            }
                            if matches!(
                                provider,
                                Provider::Google
                                    | Provider::Anthropic
                                    | Provider::AzureOpenaiResponses
                                    | Provider::AzureOpenaiChatCompletions
                            ) {
                                assert!(!headers.contains("authorization:"));
                            }
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
        (base_url, calls, server)
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
    async fn prompt_provider_controls_http_authentication_routing_and_response_parsing() {
        use std::sync::atomic::Ordering;
        for provider in Provider::ALL {
            let reason = match provider.wire_format() {
                WireFormat::Anthropic => "end_turn",
                WireFormat::Gemini => "STOP",
                WireFormat::OpenaiChat | WireFormat::MinimaxChatCompletionV2 => "stop",
                WireFormat::DashscopeText | WireFormat::DashscopeMultimodal => "stop",
                WireFormat::OpenaiResponses => "completed",
            };
            let (response, stream) = completion_fixture(provider, json!(reason));
            for (content_type, body) in [
                ("application/json", response.to_string()),
                ("text/event-stream", stream),
            ] {
                let (url, calls, server) = mock_api_server(
                    provider,
                    "prompt-key",
                    "from-extra",
                    "selected",
                    None,
                    vec![(200, content_type.into(), body)],
                )
                .await;
                // An unusable main API must not affect an independent prompt.
                let mut config = http_config("not a URL".into());
                config.rewrite.provider = if provider.wire_format() == WireFormat::OpenaiResponses {
                    Provider::Anthropic
                } else {
                    Provider::OpenaiResponses
                };
                config.rewrite.api_key.clear();
                let client = RewriteClient::new(config).unwrap();
                let prompt = RewritePrompt {
                    provider: Some(provider),
                    base_url: url,
                    api_key: "prompt-key".into(),
                    model: "prompt-model".into(),
                    prompt: "rewrite".into(),
                    extra_config: r#"{"model":"from-extra"}"#.into(),
                    ..Default::default()
                };
                let result = tokio::time::timeout(
                    Duration::from_secs(3),
                    client.execute(&prompt, "selected", &CancellationToken::new(), true),
                )
                .await
                .unwrap();
                server.abort();
                assert_eq!(
                    result.unwrap(),
                    "rewritten",
                    "{provider:?} / {content_type}"
                );
                assert_eq!(calls.load(Ordering::SeqCst), 1);
            }
        }
    }

    #[tokio::test]
    async fn inherited_prompt_execution_ignores_invalid_hidden_api_fields() {
        use std::sync::atomic::Ordering;
        let (response, _) = completion_fixture(Provider::Anthropic, json!("end_turn"));
        let (url, calls, server) = mock_api_server(
            Provider::Anthropic,
            "main-key",
            "main-model",
            "selected",
            None,
            vec![(200, "application/json".into(), response.to_string())],
        )
        .await;
        let mut config = http_config(url);
        config.rewrite.provider = Provider::Anthropic;
        config.rewrite.api_key = "main-key".into();
        config.rewrite.model = "main-model".into();
        let client = RewriteClient::new(config).unwrap();
        let prompt = RewritePrompt {
            prompt: "rewrite".into(),
            base_url: "invalid hidden URL".into(),
            api_key: "hidden-key".into(),
            model: "hidden-model".into(),
            ..Default::default()
        };
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            client.execute(&prompt, "selected", &CancellationToken::new(), true),
        )
        .await
        .unwrap();
        server.abort();
        assert_eq!(result.unwrap(), "rewritten");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn prompt_probes_use_only_api_fields_and_never_retry() {
        use std::sync::atomic::Ordering;
        for (provider, extra) in [
            (Provider::OpenaiCompatible, "{"),
            (
                Provider::OpenaiResponses,
                r#"{"model":"wrong","instructions":"wrong","draft-only":true}"#,
            ),
            (Provider::Anthropic, "[false]"),
            (
                Provider::Google,
                r#"{"model":"wrong","contents":[],"draft-only":true}"#,
            ),
        ] {
            for inherit in [false, true] {
                let (url, calls, server) = mock_api_server(
                    provider,
                    "probe-key",
                    "probe-model",
                    "Connection test.",
                    Some("Reply briefly with OK."),
                    vec![(503, "application/json".into(), "busy".into())],
                )
                .await;
                let mut config = http_config("invalid main URL".into());
                let mut prompt = RewritePrompt {
                    provider: Some(provider),
                    base_url: url.clone(),
                    api_key: "probe-key".into(),
                    model: "probe-model".into(),
                    title: String::new(),
                    prompt: "Do not transmit this real instruction".into(),
                    hotkey: "invalid hotkey".into(),
                    extra_config: extra.into(),
                    ..Default::default()
                };
                if inherit {
                    config.rewrite = RewriteConfig {
                        provider,
                        base_url: url,
                        api_key: "probe-key".into(),
                        model: "probe-model".into(),
                        prompts: Vec::new(),
                        ..Default::default()
                    };
                    prompt.provider = None;
                    prompt.base_url = "invalid hidden URL".into();
                    prompt.api_key = "hidden-key".into();
                    prompt.model = "hidden-model".into();
                }
                let client = RewriteClient::new(config).unwrap();
                assert!(prompt.validate().is_err());
                let result = tokio::time::timeout(
                    Duration::from_secs(3),
                    client.test_prompt_connection(&prompt, &CancellationToken::new()),
                )
                .await
                .unwrap();
                server.abort();
                assert!(matches!(
                    result,
                    Err(RewriteError::Request {
                        retryable: true,
                        ..
                    })
                ));
                assert_eq!(calls.load(Ordering::SeqCst), 1);
            }
        }
    }

    #[tokio::test]
    async fn canceled_prompt_probe_does_not_start_request() {
        use std::sync::atomic::Ordering;
        let (url, calls, server) =
            mock_server(vec![(503, "application/json".into(), "busy".into())]).await;
        let client = RewriteClient::new(http_config(url)).unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(matches!(
            client
                .test_prompt_connection(&RewritePrompt::default(), &cancel)
                .await,
            Err(RewriteError::Canceled)
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        server.abort();
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
            let (response, stream) = match provider.wire_format() {
                WireFormat::OpenaiResponses => (
                    json!({"status":"incomplete","output_text":"partial"}),
                    "data: {\"type\":\"response.incomplete\"}\n\n".into(),
                ),
                format => {
                    let reason = match format {
                        WireFormat::Anthropic => "max_tokens",
                        WireFormat::Gemini => "MAX_TOKENS",
                        WireFormat::OpenaiChat | WireFormat::MinimaxChatCompletionV2 => "length",
                        WireFormat::DashscopeText | WireFormat::DashscopeMultimodal => "length",
                        WireFormat::OpenaiResponses => unreachable!("handled above"),
                    };
                    completion_fixture(provider, json!(reason))
                }
            };
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
        for probe in [false, true] {
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
                let prompt = RewritePrompt {
                    prompt: "rewrite".into(),
                    ..Default::default()
                };
                if probe {
                    client.test_prompt_connection(&prompt, &task_token).await
                } else {
                    client
                        .execute(&prompt, "selected", &task_token, true)
                        .await
                        .map(|_| ())
                }
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
