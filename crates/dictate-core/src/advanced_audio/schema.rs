//! Serializable schema for Advanced Audio API workflows.
//!
//! The schema is deliberately finite.  It describes only network protocol
//! primitives which can be checked before a request is sent; it is not a
//! general workflow language.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use super::CURRENT_SCHEMA_VERSION;

/// Stored Advanced Audio API settings.  Absence in an old `config.json`
/// deserializes to this value and preserves the Legacy ASR path.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AdvancedAudioConfig {
    pub enabled: bool,
    pub workflow: Option<AdvancedAudioWorkflow>,
    pub values: BTreeMap<String, String>,
    pub secrets: BTreeMap<String, String>,
    pub remote_audio: RemoteAudioConfig,
}

impl Default for AdvancedAudioConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            workflow: None,
            values: BTreeMap::new(),
            secrets: BTreeMap::new(),
            remote_audio: RemoteAudioConfig::None,
        }
    }
}

impl fmt::Debug for AdvancedAudioConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdvancedAudioConfig")
            .field("enabled", &self.enabled)
            .field("workflow", &self.workflow)
            .field("values", &self.values)
            .field("secrets", &"<redacted>")
            .field("remote_audio", &self.remote_audio)
            .finish()
    }
}

/// A complete versioned protocol description.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdvancedAudioWorkflow {
    pub schema_version: WorkflowSchemaVersion,
    pub name: String,
    #[serde(default)]
    pub parameters: Vec<ParameterDefinition>,
    #[serde(default)]
    pub secrets: Vec<SecretDefinition>,
    pub audio: AudioSpec,
    pub recognition: AdvancedRecognition,
}

/// Kept as a newtype so error messages can mention the supplied number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkflowSchemaVersion(pub u32);

impl Default for WorkflowSchemaVersion {
    fn default() -> Self {
        Self(CURRENT_SCHEMA_VERSION)
    }
}

/// User-entered non-secret parameter declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParameterDefinition {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

/// User-entered secret declaration.  Values live separately in config and are
/// never embedded in a workflow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretDefinition {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioSpec {
    pub delivery: AudioDelivery,
    #[serde(default)]
    pub mime: Option<String>,
}

/// Audio delivery type and optional metadata.  `provider_upload` is modeled as
/// an explicit HTTP prepare stage in an async workflow; this enum simply marks
/// that the resulting capture is the audio reference sent to recognition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AudioDelivery {
    MultipartFile,
    RawAudio,
    Base64,
    DataUri,
    PublicHttpsUrl,
    CloudUri,
    ProviderUpload,
    RealtimeChunks,
}

impl AudioDelivery {
    pub const fn kind(&self) -> AudioDeliveryType {
        match self {
            Self::MultipartFile => AudioDeliveryType::MultipartFile,
            Self::RawAudio => AudioDeliveryType::RawAudio,
            Self::Base64 => AudioDeliveryType::Base64,
            Self::DataUri => AudioDeliveryType::DataUri,
            Self::PublicHttpsUrl => AudioDeliveryType::PublicHttpsUrl,
            Self::CloudUri => AudioDeliveryType::CloudUri,
            Self::ProviderUpload => AudioDeliveryType::ProviderUpload,
            Self::RealtimeChunks => AudioDeliveryType::RealtimeChunks,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioDeliveryType {
    MultipartFile,
    RawAudio,
    Base64,
    DataUri,
    PublicHttpsUrl,
    CloudUri,
    ProviderUpload,
    RealtimeChunks,
}

/// The four bounded recognition flows understood by the runtime.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum AdvancedRecognition {
    Request {
        request: Box<HttpStage>,
        final_text: ResponseExtractor,
    },
    RequestStream {
        request: Box<HttpStage>,
        stream: StreamResponse,
    },
    AsyncPoll {
        #[serde(default)]
        prepare: Option<Box<HttpStage>>,
        submit: Box<HttpStage>,
        #[serde(default)]
        poll: Option<Box<PollStage>>,
        #[serde(default)]
        result_steps: Vec<HttpStage>,
        final_text: ResponseExtractor,
    },
    RealtimeSession {
        realtime: Box<RealtimeWorkflow>,
    },
}

impl AdvancedRecognition {
    pub const fn mode_name(&self) -> &'static str {
        match self {
            Self::Request { .. } => "request",
            Self::RequestStream { .. } => "request_stream",
            Self::AsyncPoll { .. } => "async_poll",
            Self::RealtimeSession { .. } => "realtime_session",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HttpStage {
    pub method: HttpMethod,
    pub url: String,
    #[serde(default)]
    pub query: BTreeMap<String, String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub body: HttpBody,
    #[serde(default = "default_accepted_statuses")]
    pub accepted_statuses: Vec<u16>,
    #[serde(default)]
    pub signer: SignerConfig,
    #[serde(default)]
    pub captures: Vec<Capture>,
}

fn default_accepted_statuses() -> Vec<u16> {
    vec![200]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl HttpMethod {
    pub const fn is_read_only(self) -> bool {
        matches!(self, Self::Get)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HttpBody {
    #[default]
    None,
    Json {
        value: serde_json::Value,
    },
    FormUrlencoded {
        fields: BTreeMap<String, String>,
    },
    Multipart {
        fields: Vec<MultipartField>,
    },
    RawAudio,
    RawBytes {
        value: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MultipartField {
    pub name: String,
    #[serde(flatten)]
    pub value: MultipartValue,
}

/// Multipart is deliberately typed: binary audio cannot be faked by putting a
/// local path into a text template.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MultipartValue {
    Text { value: String },
    AudioFile,
    Bytes { value: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseExtractor {
    JsonPath { path: String },
    Header { name: String },
    PlainBody,
    Status,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capture {
    pub id: String,
    pub from: ResponseExtractor,
    #[serde(default)]
    pub sensitive: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamResponse {
    pub format: StreamFormat,
    #[serde(default)]
    pub rules: Vec<StreamRule>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamFormat {
    Sse,
    Ndjson,
    JsonChunks,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamRule {
    /// For SSE this selects `event:`; omission matches every event.
    #[serde(default)]
    pub event: Option<String>,
    /// A JSONPath evaluated against the decoded event, when an action needs
    /// text or a boolean/status condition.
    #[serde(default)]
    pub path: Option<String>,
    pub action: StreamAction,
    /// Optional literal match on the extractor result.  Without it a matching
    /// event invokes the action.
    #[serde(default)]
    pub equals: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamAction {
    Ignore,
    AppendDelta,
    ReplacePartial,
    CommitSegment,
    SetFinalText,
    Complete,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PollStage {
    pub request: HttpStage,
    pub interval_ms: u64,
    pub timeout_ms: u64,
    #[serde(default)]
    pub pending: Vec<PollCondition>,
    #[serde(default)]
    pub success: Vec<PollCondition>,
    #[serde(default)]
    pub failure: Vec<PollCondition>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PollCondition {
    pub from: ResponseExtractor,
    pub operator: PollOperator,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub values: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PollOperator {
    Eq,
    Ne,
    Exists,
    NotExists,
    IsTrue,
    IsFalse,
    In,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SignerConfig {
    #[default]
    None,
    AwsSigv4 {
        region: String,
        service: String,
        access_key_secret: String,
        secret_key_secret: String,
        #[serde(default)]
        session_token_secret: Option<String>,
    },
    TencentTc3 {
        service: String,
        secret_id_secret: String,
        secret_key_secret: String,
    },
}

/// Realtime is intentionally separate from generic HTTP stages.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RealtimeWorkflow {
    pub transport: RealtimeTransport,
    pub connect: RealtimeConnect,
    #[serde(default)]
    pub initial_messages: Vec<RealtimeMessage>,
    pub audio_stream: RealtimeAudioStream,
    pub audio_message: RealtimeAudioMessage,
    #[serde(default)]
    pub receive_rules: Vec<StreamRule>,
    #[serde(default)]
    pub finish_messages: Vec<RealtimeMessage>,
    pub completion: RealtimeCompletion,
    #[serde(default)]
    pub pause_behavior: PauseBehavior,
    #[serde(default = "default_finalization_timeout_ms")]
    pub finalization_timeout_ms: u64,
}

fn default_finalization_timeout_ms() -> u64 {
    15_000
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RealtimeTransport {
    WebSocket,
    Grpc,
    Http2EventStream,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RealtimeConnect {
    pub url: String,
    #[serde(default)]
    pub query: BTreeMap<String, String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub signer: SignerConfig,
    #[serde(default)]
    pub subprotocol: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RealtimeMessage {
    Json { value: serde_json::Value },
    Text { value: String },
    Binary { value: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealtimeAudioStream {
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub chunk_duration_ms: u32,
    #[serde(default)]
    pub pacing: RealtimePacing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RealtimePacing {
    #[default]
    Realtime,
    Unbounded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RealtimeAudioMessage {
    Binary,
    Text { value: String },
    Json { value: serde_json::Value },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealtimeCompletion {
    #[serde(default)]
    pub event: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub equals: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PauseBehavior {
    #[default]
    RestartSession,
    KeepSession,
}

/// Optional remote hosting for workflows that need a public HTTPS URL or a
/// cloud URI.  Credentials remain in the secret-bearing config object, never
/// in a workflow JSON document.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RemoteAudioConfig {
    #[default]
    None,
    Webdav(WebDavRemoteAudioConfig),
    S3Compatible(S3RemoteAudioConfig),
    AliyunOss(AliyunOssRemoteAudioConfig),
}

impl fmt::Debug for RemoteAudioConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::None => "None",
            Self::Webdav(_) => "Webdav(<redacted>)",
            Self::S3Compatible(_) => "S3Compatible(<redacted>)",
            Self::AliyunOss(_) => "AliyunOss(<redacted>)",
        };
        formatter.write_str(name)
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WebDavRemoteAudioConfig {
    pub upload_base_url: String,
    pub username: String,
    pub password: String,
    pub remote_path_prefix: String,
    pub public_download_base_url: String,
    pub delete_after_recognition: bool,
}

impl Default for WebDavRemoteAudioConfig {
    fn default() -> Self {
        Self {
            upload_base_url: String::new(),
            username: String::new(),
            password: String::new(),
            remote_path_prefix: String::new(),
            public_download_base_url: String::new(),
            delete_after_recognition: true,
        }
    }
}

impl fmt::Debug for WebDavRemoteAudioConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("WebDavRemoteAudioConfig(<redacted>)")
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct S3RemoteAudioConfig {
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub access_key: String,
    pub secret_key: String,
    pub prefix: String,
    pub public_url_base: Option<String>,
    pub presigned: bool,
    pub delete_after_recognition: bool,
}

impl Default for S3RemoteAudioConfig {
    fn default() -> Self {
        Self {
            endpoint: String::new(),
            region: String::new(),
            bucket: String::new(),
            access_key: String::new(),
            secret_key: String::new(),
            prefix: String::new(),
            public_url_base: None,
            presigned: false,
            delete_after_recognition: true,
        }
    }
}

impl fmt::Debug for S3RemoteAudioConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("S3RemoteAudioConfig(<redacted>)")
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AliyunOssRemoteAudioConfig {
    pub endpoint: String,
    pub bucket: String,
    pub access_key: String,
    pub secret_key: String,
    pub prefix: String,
    pub public_url_base: Option<String>,
    pub presigned: bool,
    pub delete_after_recognition: bool,
}

impl Default for AliyunOssRemoteAudioConfig {
    fn default() -> Self {
        Self {
            endpoint: String::new(),
            bucket: String::new(),
            access_key: String::new(),
            secret_key: String::new(),
            prefix: String::new(),
            public_url_base: None,
            presigned: false,
            delete_after_recognition: true,
        }
    }
}

impl fmt::Debug for AliyunOssRemoteAudioConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AliyunOssRemoteAudioConfig(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advanced_configuration_debug_output_redacts_all_secret_bearing_storage_fields() {
        let config = AdvancedAudioConfig {
            secrets: BTreeMap::from([("api_key".into(), "workflow-secret".into())]),
            remote_audio: RemoteAudioConfig::S3Compatible(S3RemoteAudioConfig {
                endpoint: "https://storage.example.test/private".into(),
                access_key: "storage-access".into(),
                secret_key: "storage-secret".into(),
                public_url_base: Some("https://public.example.test/?signature=token".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let output = format!("{config:?}");
        for secret in [
            "workflow-secret",
            "storage.example.test",
            "storage-access",
            "storage-secret",
            "signature=token",
        ] {
            assert!(!output.contains(secret));
        }
    }
}
