use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use reqwest::multipart::{Form, Part};
use thiserror::Error;
use tokio::fs::File;
use tokio_util::io::ReaderStream;

use super::MAX_BASE64_AUDIO_BYTES;
use crate::advanced_audio::schema::{HttpBody, MultipartValue};
use crate::advanced_audio::template::{
    AudioTemplateValues, RuntimeTemplateValues, Template, TemplateContext, TemplateError,
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
        audio: &'a PreparedAudio,
        runtime: &'a RuntimeTemplateValues,
        encoded_audio: bool,
    ) -> Result<Self, BodyError> {
        Ok(Self {
            values,
            secrets,
            captures,
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
        Ok(Template::parse(input)?.render(&context)?)
    }

    pub fn render_json(&self, value: &serde_json::Value) -> Result<serde_json::Value, BodyError> {
        let context = TemplateContext {
            values: self.values,
            secrets: self.secrets,
            captures: self.captures,
            audio: &self.audio_values,
            runtime: self.runtime,
        };
        render_json_with_context(value, &context)
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

fn render_json_with_context(
    value: &serde_json::Value,
    context: &TemplateContext<'_>,
) -> Result<serde_json::Value, BodyError> {
    match value {
        serde_json::Value::String(value) => Ok(serde_json::Value::String(
            Template::parse(value)?.render(context)?,
        )),
        serde_json::Value::Array(values) => values
            .iter()
            .map(|value| render_json_with_context(value, context))
            .collect::<Result<Vec<_>, _>>()
            .map(serde_json::Value::Array),
        serde_json::Value::Object(values) => values
            .iter()
            .map(|(key, value)| Ok((key.clone(), render_json_with_context(value, context)?)))
            .collect::<Result<serde_json::Map<_, _>, BodyError>>()
            .map(serde_json::Value::Object),
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            Ok(value.clone())
        }
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
    Template(#[from] TemplateError),
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
