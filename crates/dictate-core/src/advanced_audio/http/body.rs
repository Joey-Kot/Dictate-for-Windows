use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use reqwest::multipart::{Form, Part};
use thiserror::Error;
use tokio::fs::File;
use tokio_util::io::ReaderStream;

use super::MAX_BASE64_AUDIO_BYTES;
use crate::advanced_audio::schema::{
    HttpBody, MultipartValue, ParameterDefinition, ParameterType, WorkflowSchemaVersion,
};
use crate::advanced_audio::template::{
    AudioTemplateValues, Placeholder, RuntimeTemplateValues, Template, TemplateContext,
    TemplateError, is_identifier,
};

/// A user-selected or recorder-generated audio file.  The file is only held
/// in memory for delivery forms that explicitly require Base64 or a Data URI.
#[derive(Debug, Clone)]
pub struct PreparedAudio {
    path: PathBuf,
    filename: String,
    mime: String,
    size: u64,
    public_url: Option<String>,
    cloud_uri: Option<String>,
}

impl PreparedAudio {
    pub async fn from_path(path: impl AsRef<Path>, mime: Option<&str>) -> Result<Self, BodyError> {
        let path = path.as_ref().to_path_buf();
        let metadata = tokio::fs::metadata(&path)
            .await
            .map_err(|source| BodyError::AudioFile {
                path: path.clone(),
                source,
            })?;
        if !metadata.is_file() {
            return Err(BodyError::NotAFile { path });
        }
        let filename = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "audio".into());
        Ok(Self {
            path,
            filename,
            mime: mime.unwrap_or("application/octet-stream").to_owned(),
            size: metadata.len(),
            public_url: None,
            cloud_uri: None,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn filename(&self) -> &str {
        &self.filename
    }

    pub fn mime(&self) -> &str {
        &self.mime
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn with_remote_reference(
        mut self,
        public_url: Option<String>,
        cloud_uri: Option<String>,
    ) -> Self {
        self.public_url = public_url;
        self.cloud_uri = cloud_uri;
        self
    }

    pub async fn template_values(&self, encoded: bool) -> Result<AudioTemplateValues, BodyError> {
        let mut values = AudioTemplateValues {
            filename: Some(self.filename.clone()),
            mime: Some(self.mime.clone()),
            size: Some(self.size.to_string()),
            public_url: self.public_url.clone(),
            cloud_uri: self.cloud_uri.clone(),
            ..Default::default()
        };
        if encoded {
            if self.size > MAX_BASE64_AUDIO_BYTES {
                return Err(BodyError::AudioTooLargeForBase64 {
                    size: self.size,
                    maximum: MAX_BASE64_AUDIO_BYTES,
                });
            }
            let bytes =
                tokio::fs::read(&self.path)
                    .await
                    .map_err(|source| BodyError::AudioFile {
                        path: self.path.clone(),
                        source,
                    })?;
            let base64 = STANDARD.encode(bytes);
            values.data_uri = Some(format!("data:{};base64,{base64}", self.mime));
            values.base64 = Some(base64);
        }
        Ok(values)
    }

    async fn streaming_part(&self) -> Result<Part, BodyError> {
        let file = File::open(&self.path)
            .await
            .map_err(|source| BodyError::AudioFile {
                path: self.path.clone(),
                source,
            })?;
        let body = reqwest::Body::wrap_stream(ReaderStream::new(file));
        let part = Part::stream(body)
            .file_name(self.filename.clone())
            .mime_str(&self.mime)
            .map_err(BodyError::Multipart)?;
        Ok(part)
    }

    async fn streaming_body(&self) -> Result<reqwest::Body, BodyError> {
        let file = File::open(&self.path)
            .await
            .map_err(|source| BodyError::AudioFile {
                path: self.path.clone(),
                source,
            })?;
        Ok(reqwest::Body::wrap_stream(ReaderStream::new(file)))
    }
}

/// Inputs available while one HTTP stage is rendered.  Captures are carried
/// by the caller in a mutable map, but this structure keeps stage building
/// read-only.
#[derive(Debug)]
pub struct StageContext<'a> {
    pub values: &'a BTreeMap<String, String>,
    pub secrets: &'a BTreeMap<String, String>,
    pub captures: &'a BTreeMap<String, String>,
    pub parameters: &'a [ParameterDefinition],
    pub schema_version: WorkflowSchemaVersion,
    pub audio: &'a PreparedAudio,
    pub runtime: &'a RuntimeTemplateValues,
    audio_values: AudioTemplateValues,
}

impl<'a> StageContext<'a> {
    /// Renders one stage against a single audio-template snapshot.
    ///
    /// Base64 and data-URI audio must be read into memory by design.  Building
    /// that value once here prevents a workflow with URL, header, and JSON
    /// templates from repeatedly reading and encoding the same file.
    pub async fn new(
        values: &'a BTreeMap<String, String>,
        secrets: &'a BTreeMap<String, String>,
        captures: &'a BTreeMap<String, String>,
        parameters: &'a [ParameterDefinition],
        schema_version: WorkflowSchemaVersion,
        audio: &'a PreparedAudio,
        runtime: &'a RuntimeTemplateValues,
        encoded_audio: bool,
    ) -> Result<Self, BodyError> {
        Ok(Self {
            values,
            secrets,
            captures,
            parameters,
            schema_version,
            audio,
            runtime,
            audio_values: audio.template_values(encoded_audio).await?,
        })
    }

    pub fn render(&self, input: &str) -> Result<String, BodyError> {
        let context = TemplateContext {
            values: self.values,
            secrets: self.secrets,
            captures: self.captures,
            audio: &self.audio_values,
            runtime: self.runtime,
        };
        render_text_with_context(input, &context, self.parameters, self.schema_version)
            .map_err(map_template_render_error)
    }

    pub fn render_json(&self, value: &serde_json::Value) -> Result<serde_json::Value, BodyError> {
        let context = TemplateContext {
            values: self.values,
            secrets: self.secrets,
            captures: self.captures,
            audio: &self.audio_values,
            runtime: self.runtime,
        };
        render_json_with_context(value, &context, self.parameters, self.schema_version)
            .map_err(map_template_render_error)
    }
}

pub(crate) async fn build_body(
    body: &HttpBody,
    context: &StageContext<'_>,
) -> Result<BuiltBody, BodyError> {
    match body {
        HttpBody::None => Ok(BuiltBody::Empty),
        HttpBody::Json { value } => Ok(BuiltBody::Json(context.render_json(value)?)),
        HttpBody::FormUrlencoded { fields } => {
            let mut rendered = Vec::with_capacity(fields.len());
            for (key, value) in fields {
                rendered.push((key.clone(), context.render(value)?));
            }
            Ok(BuiltBody::Form(rendered))
        }
        HttpBody::Multipart { fields } => {
            let mut form = Form::new();
            for field in fields {
                form = match &field.value {
                    MultipartValue::Text { value } => {
                        form.text(field.name.clone(), context.render(value)?)
                    }
                    MultipartValue::Bytes { value } => {
                        let value = context.render(value)?;
                        let bytes = STANDARD
                            .decode(value)
                            .map_err(BodyError::InvalidBase64Bytes)?;
                        form.part(field.name.clone(), Part::bytes(bytes))
                    }
                    MultipartValue::AudioFile => {
                        form.part(field.name.clone(), context.audio.streaming_part().await?)
                    }
                };
            }
            Ok(BuiltBody::Multipart(form))
        }
        HttpBody::RawAudio => Ok(BuiltBody::RawAudio(context.audio.streaming_body().await?)),
        HttpBody::RawBytes { value } => {
            Ok(BuiltBody::RawBytes(context.render(value)?.into_bytes()))
        }
    }
}

pub(crate) enum BuiltBody {
    Empty,
    Json(serde_json::Value),
    Form(Vec<(String, String)>),
    Multipart(Form),
    RawAudio(reqwest::Body),
    RawBytes(Vec<u8>),
}

/// Renders a JSON template without turning a typed parameter into a string
/// when it occupies an entire JSON leaf.  The same helper is shared by HTTP
/// and WebSocket message rendering so a workflow cannot send a number through
/// one transport and a quoted number through another.
pub(crate) fn render_json_with_context(
    value: &serde_json::Value,
    context: &TemplateContext<'_>,
    parameters: &[ParameterDefinition],
    schema_version: WorkflowSchemaVersion,
) -> Result<serde_json::Value, TypedTemplateRenderError> {
    match value {
        serde_json::Value::String(value) => {
            render_json_string(value, context, parameters, schema_version)
        }
        serde_json::Value::Array(values) => values
            .iter()
            .map(|value| render_json_with_context(value, context, parameters, schema_version))
            .collect::<Result<Vec<_>, _>>()
            .map(serde_json::Value::Array),
        serde_json::Value::Object(values) => values
            .iter()
            .map(|(key, value)| {
                Ok((
                    key.clone(),
                    render_json_with_context(value, context, parameters, schema_version)?,
                ))
            })
            .collect::<Result<serde_json::Map<_, _>, TypedTemplateRenderError>>()
            .map(serde_json::Value::Object),
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            Ok(value.clone())
        }
    }
}

/// Renders a normal string template. Structured parameters intentionally
/// have no text serialization convention: they are valid only as an entire
/// JSON leaf, where their typed renderer can produce an array or object.
pub(crate) fn render_text_with_context(
    value: &str,
    context: &TemplateContext<'_>,
    parameters: &[ParameterDefinition],
    schema_version: WorkflowSchemaVersion,
) -> Result<String, TypedTemplateRenderError> {
    let template = Template::parse(value)?;
    reject_structured_parameter_text_use(&template, parameters, schema_version)?;
    Ok(template.render(context)?)
}

fn render_json_string(
    value: &str,
    context: &TemplateContext<'_>,
    parameters: &[ParameterDefinition],
    schema_version: WorkflowSchemaVersion,
) -> Result<serde_json::Value, TypedTemplateRenderError> {
    let template = Template::parse(value)?;
    if let Some(id) = whole_parameter_reference(value)
        && let Some(parameter) = parameters
            .iter()
            .find(|parameter| parameter.id.as_str() == id)
    {
        let stored = template.render(context)?;
        return parameter
            .parse_value(schema_version, &stored)
            .map_err(|error| TypedTemplateRenderError::InvalidParameterValue {
                id: parameter.id.clone(),
                reason: error.to_string(),
            });
    }

    // Structured parameters are deliberately JSON-leaf-only values. Rendering
    // one into a larger JSON string would require inventing a serialization
    // convention, so reject that shape instead of silently producing a
    // provider-dependent string.
    reject_structured_parameter_text_use(&template, parameters, schema_version)?;

    Ok(serde_json::Value::String(template.render(context)?))
}

fn reject_structured_parameter_text_use(
    template: &Template,
    parameters: &[ParameterDefinition],
    schema_version: WorkflowSchemaVersion,
) -> Result<(), TypedTemplateRenderError> {
    if let Some(parameter) = template.placeholders().find_map(|placeholder| {
        let Placeholder::Var(id) = placeholder else {
            return None;
        };
        parameters.iter().find(|parameter| {
            parameter.id.as_str() == id.as_str()
                && matches!(
                    parameter.effective_type(schema_version),
                    Ok(ParameterType::MultiSelect
                        | ParameterType::JsonObject
                        | ParameterType::JsonArray)
                )
        })
    }) {
        return Err(
            TypedTemplateRenderError::StructuredParameterRequiresJsonLeaf {
                id: parameter.id.clone(),
            },
        );
    }
    Ok(())
}

fn whole_parameter_reference(value: &str) -> Option<&str> {
    let id = value.strip_prefix("{{var:")?.strip_suffix("}}")?;
    is_identifier(id).then_some(id)
}

/// Error returned while rendering a typed template. It intentionally never
/// includes a stored parameter value, which can be provider-sensitive even
/// when it is not a declared secret.
#[derive(Debug, Error)]
pub(crate) enum TypedTemplateRenderError {
    #[error("{0}")]
    Template(#[from] TemplateError),
    #[error("structured parameter '{id}' may only be used as a complete JSON leaf")]
    StructuredParameterRequiresJsonLeaf { id: String },
    #[error("invalid value for parameter '{id}': {reason}")]
    InvalidParameterValue { id: String, reason: String },
}

fn map_template_render_error(error: TypedTemplateRenderError) -> BodyError {
    match error {
        TypedTemplateRenderError::Template(error) => BodyError::Template(error),
        error => BodyError::TypedTemplate(error.to_string()),
    }
}

#[derive(Debug, Error)]
pub enum BodyError {
    #[error("failed to access audio file '{path}': {source}")]
    AudioFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("audio path '{path}' is not a file")]
    NotAFile { path: PathBuf },
    #[error(
        "audio file is {size} bytes; Base64 and Data URI inputs are limited to {maximum} bytes"
    )]
    AudioTooLargeForBase64 { size: u64, maximum: u64 },
    #[error("invalid Base64 multipart bytes: {0}")]
    InvalidBase64Bytes(base64::DecodeError),
    #[error("invalid multipart field: {0}")]
    Multipart(reqwest::Error),
    #[error("{0}")]
    TypedTemplate(String),
    #[error("{0}")]
    Template(#[from] TemplateError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::advanced_audio::schema::{MultipartField, ParameterOption, ParameterType};
    use crate::advanced_audio::{CURRENT_SCHEMA_VERSION, LEGACY_SCHEMA_VERSION};

    fn parameter(id: &str, parameter_type: ParameterType) -> ParameterDefinition {
        ParameterDefinition {
            id: id.into(),
            label: id.into(),
            required: false,
            default: None,
            description: None,
            parameter_type: Some(parameter_type),
            options: vec![],
            visible_when: None,
        }
    }

    fn template_context<'a>(
        values: &'a BTreeMap<String, String>,
        secrets: &'a BTreeMap<String, String>,
        captures: &'a BTreeMap<String, String>,
        audio: &'a AudioTemplateValues,
        runtime: &'a RuntimeTemplateValues,
    ) -> TemplateContext<'a> {
        TemplateContext {
            values,
            secrets,
            captures,
            audio,
            runtime,
        }
    }

    #[tokio::test]
    async fn base64_and_data_uri_reject_audio_larger_than_the_memory_limit() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("large.wav");
        let file = std::fs::File::create(&path).unwrap();
        // Sparse allocation exercises the metadata guard without allocating
        // the entire rejected input during the test.
        file.set_len(MAX_BASE64_AUDIO_BYTES + 1).unwrap();
        drop(file);

        let audio = PreparedAudio::from_path(&path, Some("audio/wav"))
            .await
            .unwrap();
        assert!(matches!(
            audio.template_values(true).await,
            Err(BodyError::AudioTooLargeForBase64 {
                size,
                maximum: MAX_BASE64_AUDIO_BYTES,
            }) if size == MAX_BASE64_AUDIO_BYTES + 1
        ));
    }

    #[test]
    fn json_leaf_parameters_keep_their_declared_v2_json_types() {
        let values = BTreeMap::from([
            ("sample_rate".into(), "16000".into()),
            ("temperature".into(), "0.25".into()),
            ("timestamps".into(), "true".into()),
            ("languages".into(), r#"["zh","en"]"#.into()),
            ("model".into(), "flash".into()),
            (
                "vocabulary".into(),
                r#"{"wake_phrase":"redacted-vocabulary","boost":2}"#.into(),
            ),
            (
                "language_hints".into(),
                r#"[{"locale":"zh-CN"},"en-US"]"#.into(),
            ),
        ]);
        let secrets = BTreeMap::from([("key".into(), "secret".into())]);
        let captures = BTreeMap::new();
        let audio = AudioTemplateValues::default();
        let runtime = RuntimeTemplateValues::default();
        let mut languages = parameter("languages", ParameterType::MultiSelect);
        languages.options = vec![
            ParameterOption {
                value: "zh".into(),
                label: "Chinese".into(),
            },
            ParameterOption {
                value: "en".into(),
                label: "English".into(),
            },
        ];
        let parameters = vec![
            parameter("sample_rate", ParameterType::Integer),
            parameter("temperature", ParameterType::Number),
            parameter("timestamps", ParameterType::Boolean),
            languages,
            parameter("model", ParameterType::Text),
            parameter("vocabulary", ParameterType::JsonObject),
            parameter("language_hints", ParameterType::JsonArray),
        ];
        let context = template_context(&values, &secrets, &captures, &audio, &runtime);

        let rendered = render_json_with_context(
            &serde_json::json!({
                "sample_rate": "{{var:sample_rate}}",
                "temperature": "{{var:temperature}}",
                "timestamps": "{{var:timestamps}}",
                "languages": "{{var:languages}}",
                "model": "{{var:model}}",
                "vocabulary": "{{var:vocabulary}}",
                "language_hints": "{{var:language_hints}}",
                "summary": "rate={{var:sample_rate}}, model={{var:model}}",
                "api_key": "{{secret:key}}",
            }),
            &context,
            &parameters,
            WorkflowSchemaVersion(CURRENT_SCHEMA_VERSION),
        )
        .unwrap();

        assert_eq!(
            rendered,
            serde_json::json!({
                "sample_rate": 16000,
                "temperature": 0.25,
                "timestamps": true,
                "languages": ["zh", "en"],
                "model": "flash",
                "vocabulary": {"wake_phrase": "redacted-vocabulary", "boost": 2},
                "language_hints": [{"locale": "zh-CN"}, "en-US"],
                "summary": "rate=16000, model=flash",
                "api_key": "secret",
            })
        );
    }

    #[test]
    fn legacy_workflows_keep_complete_parameter_leaves_as_strings() {
        let values = BTreeMap::from([("sample_rate".into(), "16000".into())]);
        let secrets = BTreeMap::new();
        let captures = BTreeMap::new();
        let audio = AudioTemplateValues::default();
        let runtime = RuntimeTemplateValues::default();
        let context = template_context(&values, &secrets, &captures, &audio, &runtime);
        let parameters = vec![parameter("sample_rate", ParameterType::Integer)];

        let rendered = render_json_with_context(
            &serde_json::json!({"sample_rate": "{{var:sample_rate}}"}),
            &context,
            &parameters,
            WorkflowSchemaVersion(LEGACY_SCHEMA_VERSION),
        )
        .unwrap();

        assert_eq!(rendered, serde_json::json!({"sample_rate": "16000"}));
    }

    #[test]
    fn multi_select_cannot_be_embedded_in_a_json_string() {
        let values = BTreeMap::from([("languages".into(), r#"["zh","en"]"#.into())]);
        let secrets = BTreeMap::new();
        let captures = BTreeMap::new();
        let audio = AudioTemplateValues::default();
        let runtime = RuntimeTemplateValues::default();
        let context = template_context(&values, &secrets, &captures, &audio, &runtime);
        let mut languages = parameter("languages", ParameterType::MultiSelect);
        languages.options = vec![
            ParameterOption {
                value: "zh".into(),
                label: "Chinese".into(),
            },
            ParameterOption {
                value: "en".into(),
                label: "English".into(),
            },
        ];

        assert!(matches!(
            render_json_with_context(
                &serde_json::json!({"language_list": "selected={{var:languages}}"}),
                &context,
                &[languages],
                WorkflowSchemaVersion(CURRENT_SCHEMA_VERSION),
            ),
            Err(TypedTemplateRenderError::StructuredParameterRequiresJsonLeaf { id }) if id == "languages"
        ));
    }

    #[test]
    fn multi_select_cannot_be_rendered_in_an_http_string_context() {
        let values = BTreeMap::from([("languages".into(), r#"["zh","en"]"#.into())]);
        let secrets = BTreeMap::new();
        let captures = BTreeMap::new();
        let audio = AudioTemplateValues::default();
        let runtime = RuntimeTemplateValues::default();
        let context = template_context(&values, &secrets, &captures, &audio, &runtime);
        let mut languages = parameter("languages", ParameterType::MultiSelect);
        languages.options = vec![
            ParameterOption {
                value: "zh".into(),
                label: "Chinese".into(),
            },
            ParameterOption {
                value: "en".into(),
                label: "English".into(),
            },
        ];

        assert!(matches!(
            render_text_with_context(
                "https://asr.example.test/v1?languages={{var:languages}}",
                &context,
                &[languages],
                WorkflowSchemaVersion(CURRENT_SCHEMA_VERSION),
            ),
            Err(TypedTemplateRenderError::StructuredParameterRequiresJsonLeaf { id }) if id == "languages"
        ));
    }

    #[test]
    fn json_object_and_array_cannot_be_embedded_in_json_strings() {
        let values = BTreeMap::from([
            (
                "vocabulary".into(),
                r#"{"wake_phrase":"redacted-vocabulary"}"#.into(),
            ),
            (
                "language_hints".into(),
                r#"["redacted-language-hint"]"#.into(),
            ),
        ]);
        let secrets = BTreeMap::new();
        let captures = BTreeMap::new();
        let audio = AudioTemplateValues::default();
        let runtime = RuntimeTemplateValues::default();
        let context = template_context(&values, &secrets, &captures, &audio, &runtime);
        let parameters = [
            parameter("vocabulary", ParameterType::JsonObject),
            parameter("language_hints", ParameterType::JsonArray),
        ];

        for (id, stored) in [
            ("vocabulary", r#"{"wake_phrase":"redacted-vocabulary"}"#),
            ("language_hints", r#"["redacted-language-hint"]"#),
        ] {
            let error = render_json_with_context(
                &serde_json::json!({"invalid": format!("prefix {{{{var:{id}}}}}")}),
                &context,
                &parameters,
                WorkflowSchemaVersion(CURRENT_SCHEMA_VERSION),
            )
            .unwrap_err();

            let message = error.to_string();
            match error {
                TypedTemplateRenderError::StructuredParameterRequiresJsonLeaf { id: actual } => {
                    assert_eq!(actual, id)
                }
                error => panic!("expected structured parameter error, got {error:?}"),
            }
            assert!(!message.contains(stored));
        }
    }

    #[tokio::test]
    async fn structured_parameters_are_rejected_in_http_string_contexts_before_delivery() {
        let vocabulary = r#"{"wake_phrase":"redacted-vocabulary"}"#;
        let language_hints = r#"["redacted-language-hint"]"#;
        let values = BTreeMap::from([
            ("vocabulary".into(), vocabulary.into()),
            ("language_hints".into(), language_hints.into()),
        ]);
        let secrets = BTreeMap::new();
        let captures = BTreeMap::new();
        let runtime = RuntimeTemplateValues::default();
        let parameters = [
            parameter("vocabulary", ParameterType::JsonObject),
            parameter("language_hints", ParameterType::JsonArray),
        ];
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("audio.wav");
        std::fs::write(&path, [0_u8]).unwrap();
        let audio = PreparedAudio::from_path(&path, Some("audio/wav"))
            .await
            .unwrap();
        let context = StageContext::new(
            &values,
            &secrets,
            &captures,
            &parameters,
            WorkflowSchemaVersion(CURRENT_SCHEMA_VERSION),
            &audio,
            &runtime,
            false,
        )
        .await
        .unwrap();

        // URL, query, and header values all pass through the same text
        // renderer before an HTTP request can be constructed.
        for (field, template, id, stored) in [
            (
                "URL",
                "https://asr.example.test/{{var:vocabulary}}",
                "vocabulary",
                vocabulary,
            ),
            (
                "query",
                "languages={{var:language_hints}}",
                "language_hints",
                language_hints,
            ),
            (
                "header",
                "Bearer {{var:vocabulary}}",
                "vocabulary",
                vocabulary,
            ),
        ] {
            let error = context
                .render(template)
                .expect_err(&format!("{field} must fail before delivery"));
            assert_body_typed_template_error(error, id, stored);
        }

        let bodies = vec![
            (
                "form",
                HttpBody::FormUrlencoded {
                    fields: BTreeMap::from([("languages".into(), "{{var:language_hints}}".into())]),
                },
                "language_hints",
                language_hints,
            ),
            (
                "multipart text",
                HttpBody::Multipart {
                    fields: vec![MultipartField {
                        name: "vocabulary".into(),
                        value: MultipartValue::Text {
                            value: "{{var:vocabulary}}".into(),
                        },
                    }],
                },
                "vocabulary",
                vocabulary,
            ),
            (
                "multipart bytes",
                HttpBody::Multipart {
                    fields: vec![MultipartField {
                        name: "hints".into(),
                        value: MultipartValue::Bytes {
                            value: "{{var:language_hints}}".into(),
                        },
                    }],
                },
                "language_hints",
                language_hints,
            ),
            (
                "raw bytes",
                HttpBody::RawBytes {
                    value: "{{var:vocabulary}}".into(),
                },
                "vocabulary",
                vocabulary,
            ),
        ];
        for (field, body, id, stored) in bodies {
            let error = match build_body(&body, &context).await {
                Ok(_) => panic!("{field} must reject a structured parameter before delivery"),
                Err(error) => error,
            };
            assert_body_typed_template_error(error, id, stored);
        }
    }

    fn assert_body_typed_template_error(error: BodyError, id: &str, stored: &str) {
        let message = error.to_string();
        assert!(matches!(error, BodyError::TypedTemplate(_)));
        assert!(message.contains(id));
        assert!(!message.contains(stored));
    }
}
