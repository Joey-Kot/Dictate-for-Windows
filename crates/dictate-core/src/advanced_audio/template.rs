//! Small, non-Turing-complete template language for workflow strings.
//!
//! There are no expressions, functions, file reads, environment references,
//! or control flow.  A placeholder always resolves to one scalar value from a
//! fixed namespace.

use std::collections::BTreeMap;
use std::fmt;

use thiserror::Error;

/// A parsed template made of literal text and fixed placeholders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    parts: Vec<TemplatePart>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TemplatePart {
    Literal(String),
    Placeholder(Placeholder),
}

/// A fixed, safe namespace entry.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Placeholder {
    Var(String),
    Secret(String),
    Capture(String),
    Audio(AudioPlaceholder),
    Runtime(RuntimePlaceholder),
}

impl Placeholder {
    pub fn namespace(&self) -> &'static str {
        match self {
            Self::Var(_) => "var",
            Self::Secret(_) => "secret",
            Self::Capture(_) => "capture",
            Self::Audio(_) => "audio",
            Self::Runtime(_) => "runtime",
        }
    }

    /// The identifier without the namespace.
    pub fn key(&self) -> &str {
        match self {
            Self::Var(value) | Self::Secret(value) | Self::Capture(value) => value,
            Self::Audio(value) => value.as_str(),
            Self::Runtime(value) => value.as_str(),
        }
    }
}

impl fmt::Display for Placeholder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{{{{{}:{}}}}}", self.namespace(), self.key())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AudioPlaceholder {
    Filename,
    Mime,
    Size,
    Base64,
    DataUri,
    PublicUrl,
    CloudUri,
    ChunkBase64,
}

impl AudioPlaceholder {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Filename => "filename",
            Self::Mime => "mime",
            Self::Size => "size",
            Self::Base64 => "base64",
            Self::DataUri => "data_uri",
            Self::PublicUrl => "public_url",
            Self::CloudUri => "cloud_uri",
            Self::ChunkBase64 => "chunk_base64",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "filename" => Self::Filename,
            "mime" => Self::Mime,
            "size" => Self::Size,
            "base64" => Self::Base64,
            "data_uri" => Self::DataUri,
            "public_url" => Self::PublicUrl,
            "cloud_uri" => Self::CloudUri,
            "chunk_base64" => Self::ChunkBase64,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RuntimePlaceholder {
    Uuid,
    UnixSeconds,
    UnixMillis,
}

impl RuntimePlaceholder {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Uuid => "uuid",
            Self::UnixSeconds => "unix_seconds",
            Self::UnixMillis => "unix_millis",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "uuid" => Self::Uuid,
            "unix_seconds" => Self::UnixSeconds,
            "unix_millis" => Self::UnixMillis,
            _ => return None,
        })
    }
}

/// Dynamic values supplied to a template at execution time.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AudioTemplateValues {
    pub filename: Option<String>,
    pub mime: Option<String>,
    pub size: Option<String>,
    pub base64: Option<String>,
    pub data_uri: Option<String>,
    pub public_url: Option<String>,
    pub cloud_uri: Option<String>,
    pub chunk_base64: Option<String>,
}

impl AudioTemplateValues {
    fn get(&self, placeholder: AudioPlaceholder) -> Option<&str> {
        match placeholder {
            AudioPlaceholder::Filename => self.filename.as_deref(),
            AudioPlaceholder::Mime => self.mime.as_deref(),
            AudioPlaceholder::Size => self.size.as_deref(),
            AudioPlaceholder::Base64 => self.base64.as_deref(),
            AudioPlaceholder::DataUri => self.data_uri.as_deref(),
            AudioPlaceholder::PublicUrl => self.public_url.as_deref(),
            AudioPlaceholder::CloudUri => self.cloud_uri.as_deref(),
            AudioPlaceholder::ChunkBase64 => self.chunk_base64.as_deref(),
        }
    }
}

/// Runtime fields are supplied once per execution so every request in the
/// same workflow sees the same UUID/timestamps.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeTemplateValues {
    pub uuid: Option<String>,
    pub unix_seconds: Option<String>,
    pub unix_millis: Option<String>,
}

impl RuntimeTemplateValues {
    fn get(&self, placeholder: RuntimePlaceholder) -> Option<&str> {
        match placeholder {
            RuntimePlaceholder::Uuid => self.uuid.as_deref(),
            RuntimePlaceholder::UnixSeconds => self.unix_seconds.as_deref(),
            RuntimePlaceholder::UnixMillis => self.unix_millis.as_deref(),
        }
    }
}

/// All values permitted when rendering one stage.  The maps are borrowed so
/// secrets are not copied into parsed workflow structures or errors.
#[derive(Debug, Clone)]
pub struct TemplateContext<'a> {
    pub values: &'a BTreeMap<String, String>,
    pub secrets: &'a BTreeMap<String, String>,
    pub captures: &'a BTreeMap<String, String>,
    pub audio: &'a AudioTemplateValues,
    pub runtime: &'a RuntimeTemplateValues,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum TemplateError {
    #[error("unterminated template placeholder at byte {position}")]
    Unterminated { position: usize },
    #[error("unexpected closing template delimiter at byte {position}")]
    UnexpectedClose { position: usize },
    #[error("empty template placeholder at byte {position}")]
    Empty { position: usize },
    #[error("invalid template placeholder '{placeholder}'")]
    InvalidPlaceholder { placeholder: String },
    #[error("unknown template namespace '{namespace}'")]
    UnknownNamespace { namespace: String },
    #[error("unknown {namespace} placeholder '{key}'")]
    UnknownFixedPlaceholder {
        namespace: &'static str,
        key: String,
    },
    #[error("missing value for {placeholder}")]
    MissingValue { placeholder: Placeholder },
}

impl Template {
    /// Parses all placeholders eagerly.  This detects unsupported namespaces
    /// before a workflow can reach the HTTP layer.
    pub fn parse(input: &str) -> Result<Self, TemplateError> {
        let mut parts = Vec::new();
        let mut cursor = 0;
        while let Some(relative_open) = input[cursor..].find("{{") {
            let open = cursor + relative_open;
            if let Some(relative_close) = input[cursor..open].find("}}") {
                return Err(TemplateError::UnexpectedClose {
                    position: cursor + relative_close,
                });
            }
            if open > cursor {
                parts.push(TemplatePart::Literal(input[cursor..open].into()));
            }
            let content_start = open + 2;
            let Some(relative_end) = input[content_start..].find("}}") else {
                return Err(TemplateError::Unterminated { position: open });
            };
            let end = content_start + relative_end;
            let placeholder = parse_placeholder(&input[content_start..end], open)?;
            parts.push(TemplatePart::Placeholder(placeholder));
            cursor = end + 2;
        }
        if let Some(relative_close) = input[cursor..].find("}}") {
            return Err(TemplateError::UnexpectedClose {
                position: cursor + relative_close,
            });
        }
        if cursor < input.len() {
            parts.push(TemplatePart::Literal(input[cursor..].into()));
        }
        Ok(Self { parts })
    }

    pub fn placeholders(&self) -> impl Iterator<Item = &Placeholder> {
        self.parts.iter().filter_map(|part| match part {
            TemplatePart::Placeholder(placeholder) => Some(placeholder),
            TemplatePart::Literal(_) => None,
        })
    }

    pub fn render(&self, context: &TemplateContext<'_>) -> Result<String, TemplateError> {
        let mut rendered = String::new();
        for part in &self.parts {
            match part {
                TemplatePart::Literal(text) => rendered.push_str(text),
                TemplatePart::Placeholder(placeholder) => {
                    let value = match placeholder {
                        Placeholder::Var(key) => context.values.get(key).map(String::as_str),
                        Placeholder::Secret(key) => context.secrets.get(key).map(String::as_str),
                        Placeholder::Capture(key) => context.captures.get(key).map(String::as_str),
                        Placeholder::Audio(key) => context.audio.get(*key),
                        Placeholder::Runtime(key) => context.runtime.get(*key),
                    }
                    .ok_or_else(|| TemplateError::MissingValue {
                        placeholder: placeholder.clone(),
                    })?;
                    rendered.push_str(value);
                }
            }
        }
        Ok(rendered)
    }
}

fn parse_placeholder(input: &str, position: usize) -> Result<Placeholder, TemplateError> {
    if input.is_empty() {
        return Err(TemplateError::Empty { position });
    }
    if input.trim() != input || input.contains(['{', '}']) {
        return Err(TemplateError::InvalidPlaceholder {
            placeholder: input.into(),
        });
    }
    let Some((namespace, key)) = input.split_once(':') else {
        return Err(TemplateError::InvalidPlaceholder {
            placeholder: input.into(),
        });
    };
    if key.is_empty() || key.contains(':') {
        return Err(TemplateError::InvalidPlaceholder {
            placeholder: input.into(),
        });
    }
    match namespace {
        "var" if is_identifier(key) => Ok(Placeholder::Var(key.into())),
        "secret" if is_identifier(key) => Ok(Placeholder::Secret(key.into())),
        "capture" if is_identifier(key) => Ok(Placeholder::Capture(key.into())),
        "audio" => AudioPlaceholder::parse(key)
            .map(Placeholder::Audio)
            .ok_or_else(|| TemplateError::UnknownFixedPlaceholder {
                namespace: "audio",
                key: key.into(),
            }),
        "runtime" => RuntimePlaceholder::parse(key)
            .map(Placeholder::Runtime)
            .ok_or_else(|| TemplateError::UnknownFixedPlaceholder {
                namespace: "runtime",
                key: key.into(),
            }),
        "var" | "secret" | "capture" => Err(TemplateError::InvalidPlaceholder {
            placeholder: input.into(),
        }),
        unknown => Err(TemplateError::UnknownNamespace {
            namespace: unknown.into(),
        }),
    }
}

pub fn is_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context<'a>(
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

    #[test]
    fn parses_and_renders_only_fixed_namespaces() {
        let template = Template::parse(
            "{{var:model}}/{{secret:key}}/{{capture:task_id}}/{{audio:filename}}/{{runtime:uuid}}",
        )
        .unwrap();
        assert_eq!(
            template
                .placeholders()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            [
                "{{var:model}}",
                "{{secret:key}}",
                "{{capture:task_id}}",
                "{{audio:filename}}",
                "{{runtime:uuid}}",
            ]
        );
        let values = BTreeMap::from([("model".into(), "m".into())]);
        let secrets = BTreeMap::from([("key".into(), "do-not-log".into())]);
        let captures = BTreeMap::from([("task_id".into(), "job".into())]);
        let audio = AudioTemplateValues {
            filename: Some("voice.wav".into()),
            ..Default::default()
        };
        let runtime = RuntimeTemplateValues {
            uuid: Some("id".into()),
            ..Default::default()
        };
        assert_eq!(
            template
                .render(&context(&values, &secrets, &captures, &audio, &runtime))
                .unwrap(),
            "m/do-not-log/job/voice.wav/id"
        );
    }

    #[test]
    fn rejects_untrusted_or_executable_template_forms() {
        for input in [
            "{{env:HOME}}",
            "{{file:path}}",
            "{{shell:command}}",
            "{{javascript:alert}}",
            "{{var:model + 1}}",
            "{{var:model}} trailing }}",
            "{{var:model",
            "{{runtime:clock}}",
            "{{audio:path}}",
        ] {
            assert!(Template::parse(input).is_err(), "{input}");
        }
    }

    #[test]
    fn requires_every_runtime_value_to_be_supplied() {
        let template = Template::parse("{{secret:key}}").unwrap();
        let empty = BTreeMap::new();
        let error = template
            .render(&context(
                &empty,
                &empty,
                &empty,
                &AudioTemplateValues::default(),
                &RuntimeTemplateValues::default(),
            ))
            .unwrap_err();
        assert!(matches!(
            error,
            TemplateError::MissingValue {
                placeholder: Placeholder::Secret(ref key)
            } if key == "key"
        ));
    }
}
