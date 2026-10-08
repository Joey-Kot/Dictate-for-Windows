//! Structural and semantic validation for Advanced Audio API workflows.
//!
//! Validation happens before construction of a network client.  It rejects
//! unknown schema versions, unsafe template namespaces, future captures, and
//! protocol shapes the runtime cannot execute deterministically.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde_json::Value;

use super::schema::*;
use super::template::{AudioPlaceholder, Placeholder, Template, is_identifier};
use super::{CURRENT_SCHEMA_VERSION, LEGACY_SCHEMA_VERSION, is_supported_schema_version};

const MAX_WORKFLOW_NAME_BYTES: usize = 256;
const MAX_WORKFLOW_JSON_BYTES: usize = 256 * 1024;
const MAX_DECLARATIONS: usize = 64;
const MAX_TEMPLATE_BYTES: usize = 32 * 1024;
const MAX_HEADERS: usize = 64;
const MAX_QUERY: usize = 64;
const MAX_CAPTURES: usize = 64;
const MAX_MULTIPART_FIELDS: usize = 64;
const MAX_STREAM_RULES: usize = 64;
const MAX_MESSAGES: usize = 32;
const MAX_POLL_TIMEOUT_MS: u64 = 24 * 60 * 60 * 1000;
const MIN_POLL_INTERVAL_MS: u64 = 100;
const MIN_CHUNK_DURATION_MS: u32 = 10;
const MAX_CHUNK_DURATION_MS: u32 = 1_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub path: String,
    pub message: String,
}

impl ValidationError {
    fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.path, self.message)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ValidationErrors {
    errors: Vec<ValidationError>,
}

impl ValidationErrors {
    pub fn errors(&self) -> &[ValidationError] {
        &self.errors
    }

    pub fn is_empty(&self) -> bool {
        self.errors.is_empty()
    }

    fn push(&mut self, path: impl Into<String>, message: impl Into<String>) {
        self.errors.push(ValidationError::new(path, message));
    }

    fn finish(self) -> Result<(), Self> {
        if self.errors.is_empty() {
            Ok(())
        } else {
            Err(self)
        }
    }
}

impl fmt::Display for ValidationErrors {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, error) in self.errors.iter().enumerate() {
            if index != 0 {
                formatter.write_str("; ")?;
            }
            error.fmt(formatter)?;
        }
        Ok(())
    }
}

impl std::error::Error for ValidationErrors {}

/// Validates only a portable workflow document.  It does not require values
/// and secrets, allowing the GUI to validate a draft before the user fills in
/// its dynamic fields.
pub fn validate_workflow(workflow: &AdvancedAudioWorkflow) -> Result<(), ValidationErrors> {
    let mut errors = ValidationErrors::default();
    validate_workflow_into(workflow, &mut errors);
    errors.finish()
}

/// Validates a stored Advanced configuration, including enabled-state values,
/// required secrets, and remote hosting requirements.
pub fn validate_advanced_audio_config(
    config: &AdvancedAudioConfig,
) -> Result<(), ValidationErrors> {
    if !config.enabled {
        // Disabled settings are deliberately inert.  A user may keep an old
        // draft while using the legacy Audio API.
        return Ok(());
    }
    let mut errors = ValidationErrors::default();
    let Some(workflow) = &config.workflow else {
        errors.push("ADVANCED_AUDIO_API.workflow", "is required while enabled");
        return Err(errors);
    };
    validate_workflow_into(workflow, &mut errors);
    validate_config_values(config, workflow, &mut errors);
    validate_remote_audio(&config.remote_audio, workflow, &mut errors);
    errors.finish()
}

/// Validates only whether a remote-audio configuration can satisfy a
/// workflow's selected delivery form.  GUI drafts can use this without
/// inventing placeholder values for required typed parameters or secrets.
pub fn validate_remote_audio_config(
    workflow: &AdvancedAudioWorkflow,
    remote_audio: &RemoteAudioConfig,
) -> Result<(), ValidationErrors> {
    let mut errors = ValidationErrors::default();
    validate_remote_audio(remote_audio, workflow, &mut errors);
    errors.finish()
}

fn validate_workflow_into(workflow: &AdvancedAudioWorkflow, errors: &mut ValidationErrors) {
    // The persisted workflow is a declarative configuration document, not an
    // arbitrary large payload.  Measure its canonical JSON representation in
    // Core so the same limit applies to GUI drafts and hand-edited configs.
    if serde_json::to_vec(workflow).is_ok_and(|document| document.len() > MAX_WORKFLOW_JSON_BYTES) {
        errors.push(
            "workflow",
            format!("serialized workflow exceeds {MAX_WORKFLOW_JSON_BYTES} bytes"),
        );
    }
    if !is_supported_schema_version(workflow.schema_version.0) {
        errors.push(
            "schema_version",
            format!(
                "Unsupported Workflow Schema Version {}; this build supports {} and {}",
                workflow.schema_version.0, LEGACY_SCHEMA_VERSION, CURRENT_SCHEMA_VERSION
            ),
        );
    }
    validate_text(
        &workflow.name,
        "name",
        true,
        MAX_WORKFLOW_NAME_BYTES,
        errors,
    );

    let mut parameters = BTreeSet::new();
    if workflow.parameters.len() > MAX_DECLARATIONS {
        errors.push(
            "parameters",
            format!("must contain at most {MAX_DECLARATIONS} entries"),
        );
    }
    for (index, definition) in workflow.parameters.iter().enumerate() {
        let path = format!("parameters[{index}]");
        validate_definition(
            &definition.id,
            &definition.label,
            definition.description.as_deref(),
            &path,
            &mut parameters,
            errors,
        );
        validate_parameter_definition(definition, workflow.schema_version, &path, errors);
    }
    if workflow.schema_version.0 == CURRENT_SCHEMA_VERSION {
        for (index, definition) in workflow.parameters.iter().enumerate() {
            if let Some(condition) = &definition.visible_when {
                validate_visibility_condition(
                    condition,
                    &workflow.parameters[..index],
                    &format!("parameters[{index}].visible_when"),
                    errors,
                );
            }
        }
    }

    let mut secrets = BTreeSet::new();
    if workflow.secrets.len() > MAX_DECLARATIONS {
        errors.push(
            "secrets",
            format!("must contain at most {MAX_DECLARATIONS} entries"),
        );
    }
    for (index, definition) in workflow.secrets.iter().enumerate() {
        let path = format!("secrets[{index}]");
        validate_definition(
            &definition.id,
            &definition.label,
            definition.description.as_deref(),
            &path,
            &mut secrets,
            errors,
        );
    }
    for duplicate in parameters.intersection(&secrets) {
        errors.push(
            "parameters/secrets",
            format!("'{duplicate}' is declared as both a value and a secret"),
        );
    }

    let no_captures = BTreeSet::new();
    let base_scope = TemplateScope::new(
        &parameters,
        &secrets,
        &no_captures,
        workflow.audio.delivery.kind(),
        false,
    );
    match &workflow.recognition {
        AdvancedRecognition::Request {
            request,
            final_text,
        } => {
            validate_non_realtime_audio(&workflow.audio, "audio", errors);
            let mut captures = BTreeSet::new();
            validate_http_stage(
                request,
                "recognition.request",
                &base_scope,
                &mut captures,
                errors,
            );
            validate_extractor(final_text, "recognition.final_text", errors);
            validate_audio_delivery_usage(
                &workflow.audio,
                &[request.as_ref()],
                "audio.delivery",
                errors,
            );
            validate_provider_upload(None, None, &workflow.audio, errors);
        }
        AdvancedRecognition::RequestStream { request, stream } => {
            validate_non_realtime_audio(&workflow.audio, "audio", errors);
            let mut captures = BTreeSet::new();
            validate_http_stage(
                request,
                "recognition.request",
                &base_scope,
                &mut captures,
                errors,
            );
            // A streamed response has not been materialized when captures
            // are evaluated.  Only the status line and headers are stable at
            // that point; accepting a body/JSON capture here would let a
            // draft pass validation only to fail after it has sent audio.
            for (index, capture) in request.captures.iter().enumerate() {
                if !matches!(
                    capture.from,
                    ResponseExtractor::Header { .. } | ResponseExtractor::Status
                ) {
                    errors.push(
                        format!("recognition.request.captures[{index}].from"),
                        "request_stream captures may only use header or status extractors",
                    );
                }
            }
            validate_stream(stream, "recognition.stream", &base_scope, true, errors);
            validate_audio_delivery_usage(
                &workflow.audio,
                &[request.as_ref()],
                "audio.delivery",
                errors,
            );
            validate_provider_upload(None, None, &workflow.audio, errors);
        }
        AdvancedRecognition::AsyncPoll {
            prepare,
            submit,
            poll,
            result_steps,
            final_text,
        } => {
            validate_non_realtime_audio(&workflow.audio, "audio", errors);
            if result_steps.len() > 2 {
                errors.push("recognition.result_steps", "must contain at most 2 stages");
            }
            let mut captures = BTreeSet::new();
            if let Some(stage) = prepare {
                validate_http_stage(
                    stage,
                    "recognition.prepare",
                    &base_scope,
                    &mut captures,
                    errors,
                );
            }
            validate_http_stage(
                submit,
                "recognition.submit",
                &base_scope,
                &mut captures,
                errors,
            );
            if let Some(stage) = poll {
                validate_poll_stage(
                    stage,
                    "recognition.poll",
                    &base_scope,
                    &mut captures,
                    errors,
                );
            }
            for (index, stage) in result_steps.iter().enumerate() {
                validate_http_stage(
                    stage,
                    &format!("recognition.result_steps[{index}]"),
                    &base_scope,
                    &mut captures,
                    errors,
                );
            }
            validate_extractor(final_text, "recognition.final_text", errors);
            let audio_stages = prepare
                .as_deref()
                .into_iter()
                .chain(std::iter::once(submit.as_ref()))
                .collect::<Vec<_>>();
            validate_audio_delivery_usage(&workflow.audio, &audio_stages, "audio.delivery", errors);
            validate_provider_upload(
                prepare.as_deref(),
                Some(submit.as_ref()),
                &workflow.audio,
                errors,
            );
        }
        AdvancedRecognition::RealtimeSession { realtime } => {
            if !matches!(workflow.audio.delivery, AudioDelivery::RealtimeChunks) {
                errors.push(
                    "audio.delivery",
                    "realtime_session requires audio delivery type realtime_chunks",
                );
            }
            validate_realtime(realtime, "recognition.realtime", &base_scope, errors);
        }
    }
}

fn validate_definition(
    id: &str,
    label: &str,
    description: Option<&str>,
    path: &str,
    seen: &mut BTreeSet<String>,
    errors: &mut ValidationErrors,
) {
    if !is_identifier(id) {
        errors.push(format!("{path}.id"), "must be an ASCII identifier");
    } else if !seen.insert(id.into()) {
        errors.push(
            format!("{path}.id"),
            format!("duplicate declaration '{id}'"),
        );
    }
    validate_text(label, &format!("{path}.label"), true, 256, errors);
    if let Some(description) = description {
        validate_text(
            description,
            &format!("{path}.description"),
            false,
            2_048,
            errors,
        );
    }
}

fn validate_parameter_definition(
    definition: &ParameterDefinition,
    schema_version: WorkflowSchemaVersion,
    path: &str,
    errors: &mut ValidationErrors,
) {
    match schema_version.0 {
        LEGACY_SCHEMA_VERSION => {
            if definition.parameter_type.is_some() {
                errors.push(
                    format!("{path}.type"),
                    "is only supported by workflow schema version 2",
                );
            }
            if !definition.options.is_empty() {
                errors.push(
                    format!("{path}.options"),
                    "is only supported by workflow schema version 2",
                );
            }
            if definition.visible_when.is_some() {
                errors.push(
                    format!("{path}.visible_when"),
                    "is only supported by workflow schema version 2",
                );
            }
            if let Some(default) = &definition.default {
                validate_literal_default(default, &format!("{path}.default"), errors);
            }
        }
        CURRENT_SCHEMA_VERSION => {
            let Some(parameter_type) = definition.parameter_type else {
                errors.push(
                    format!("{path}.type"),
                    "is required for workflow schema version 2",
                );
                return;
            };
            validate_parameter_options(definition, parameter_type, path, errors);
            if let Some(default) = &definition.default {
                validate_literal_parameter_value(
                    definition,
                    schema_version,
                    default,
                    &format!("{path}.default"),
                    errors,
                );
            }
        }
        _ => {
            // Version support has already been reported above.  Do not infer
            // a future schema's type/default semantics from this build.
        }
    }
}

fn validate_parameter_options(
    definition: &ParameterDefinition,
    parameter_type: ParameterType,
    path: &str,
    errors: &mut ValidationErrors,
) {
    let selection = matches!(
        parameter_type,
        ParameterType::Select | ParameterType::MultiSelect
    );
    if !selection {
        if !definition.options.is_empty() {
            errors.push(
                format!("{path}.options"),
                "is allowed only for select or multi_select parameters",
            );
        }
        return;
    }
    if definition.options.is_empty() {
        errors.push(
            format!("{path}.options"),
            "must contain at least one option for a selection parameter",
        );
        return;
    }
    let mut values = BTreeSet::new();
    for (index, option) in definition.options.iter().enumerate() {
        let option_path = format!("{path}.options[{index}]");
        validate_text(
            &option.value,
            &format!("{option_path}.value"),
            true,
            256,
            errors,
        );
        validate_text(
            &option.label,
            &format!("{option_path}.label"),
            true,
            256,
            errors,
        );
        if !values.insert(option.value.clone()) {
            errors.push(
                format!("{option_path}.value"),
                format!("duplicate option value '{}'", option.value),
            );
        }
    }
}

fn validate_visibility_condition(
    condition: &VisibilityCondition,
    preceding: &[ParameterDefinition],
    path: &str,
    errors: &mut ValidationErrors,
) {
    if !is_identifier(&condition.parameter) {
        errors.push(format!("{path}.parameter"), "must be an ASCII identifier");
    }
    let has_equals = condition.equals.is_some();
    let has_one_of = !condition.one_of.is_empty();
    if has_equals == has_one_of {
        errors.push(
            path,
            "must contain exactly one of equals or a nonempty one_of array",
        );
        return;
    }
    let Some(source) = preceding
        .iter()
        .rev()
        .find(|definition| definition.id == condition.parameter)
    else {
        errors.push(
            format!("{path}.parameter"),
            "must reference an earlier unconditional boolean or select parameter",
        );
        return;
    };
    if source.visible_when.is_some() {
        errors.push(
            format!("{path}.parameter"),
            "must reference an unconditional parameter",
        );
    }
    if source.default.is_none() {
        errors.push(
            format!("{path}.parameter"),
            "must reference a parameter with a default value",
        );
    }
    let Some(parameter_type) = source.parameter_type else {
        errors.push(
            format!("{path}.parameter"),
            "must reference a valid version-2 boolean or select parameter",
        );
        return;
    };
    if !matches!(
        parameter_type,
        ParameterType::Boolean | ParameterType::Select
    ) {
        errors.push(
            format!("{path}.parameter"),
            "must reference a boolean or select parameter",
        );
        return;
    }
    let values = condition
        .equals
        .as_deref()
        .into_iter()
        .chain(condition.one_of.iter().map(String::as_str));
    let mut seen = BTreeSet::new();
    for (index, value) in values.enumerate() {
        let value_path = if has_equals {
            format!("{path}.equals")
        } else {
            format!("{path}.one_of[{index}]")
        };
        if !seen.insert(value) {
            errors.push(value_path, "must not contain duplicate comparison values");
            continue;
        }
        let valid = match parameter_type {
            ParameterType::Boolean => matches!(value, "true" | "false"),
            ParameterType::Select => source.options.iter().any(|option| option.value == value),
            ParameterType::Text
            | ParameterType::Integer
            | ParameterType::Number
            | ParameterType::MultiSelect
            | ParameterType::JsonObject
            | ParameterType::JsonArray => false,
        };
        if !valid {
            errors.push(
                value_path,
                "must be a declared value of the referenced parameter",
            );
        }
    }
}

/// Defaults are persisted user values, not another evaluation phase.  Keeping
/// them literal prevents a configuration that validates but later inserts an
/// unrendered `{{runtime:*}}` or other placeholder into a request.
fn validate_literal_default(value: &str, path: &str, errors: &mut ValidationErrors) {
    if value.len() > MAX_TEMPLATE_BYTES {
        errors.push(path, format!("template exceeds {MAX_TEMPLATE_BYTES} bytes"));
        return;
    }
    match Template::parse(value) {
        Ok(template) if template.placeholders().next().is_some() => errors.push(
            path,
            "must be a literal; parameter defaults may not contain template placeholders",
        ),
        Ok(_) => {}
        Err(error) => errors.push(path, error.to_string()),
    }
}

fn validate_literal_parameter_value(
    definition: &ParameterDefinition,
    schema_version: WorkflowSchemaVersion,
    value: &str,
    path: &str,
    errors: &mut ValidationErrors,
) {
    // A text/select default is still a stored string, so it must not become
    // a hidden second template-evaluation phase. Structured JSON values are
    // parsed once as complete leaves and are never text-rendered, so their
    // contents remain literal JSON data.
    if matches!(
        definition.effective_type(schema_version),
        Ok(ParameterType::Text | ParameterType::Select)
    ) {
        validate_literal_default(value, path, errors);
    }
    if let Err(error) = definition.parse_value(schema_version, value) {
        errors.push(path, error.to_string());
    }
}

fn validate_config_values(
    config: &AdvancedAudioConfig,
    workflow: &AdvancedAudioWorkflow,
    errors: &mut ValidationErrors,
) {
    let values: BTreeSet<_> = config.values.keys().cloned().collect();
    let secrets: BTreeSet<_> = config.secrets.keys().cloned().collect();
    for parameter in &workflow.parameters {
        let value_path = format!("ADVANCED_AUDIO_API.values.{}", parameter.id);
        let configured = config.values.get(&parameter.id);
        let effective = configured.or(parameter.default.as_ref());
        match effective {
            Some(value) => {
                if workflow.schema_version.0 == CURRENT_SCHEMA_VERSION {
                    if let Err(error) = parameter.parse_value(workflow.schema_version, value) {
                        errors.push(&value_path, error.to_string());
                    }
                    if parameter.required && is_empty_required_value(parameter, value) {
                        errors.push(&value_path, "required value must not be empty");
                    }
                }
            }
            None if parameter.required => errors.push(value_path, "required value is missing"),
            None => {}
        }
    }
    for secret in &workflow.secrets {
        if secret.required
            && config
                .secrets
                .get(&secret.id)
                .is_none_or(|value| value.is_empty())
        {
            errors.push(
                format!("ADVANCED_AUDIO_API.secrets.{}", secret.id),
                "required secret is missing",
            );
        }
    }
    let declared_values: BTreeSet<_> = workflow
        .parameters
        .iter()
        .map(|entry| entry.id.clone())
        .collect();
    let declared_secrets: BTreeSet<_> = workflow
        .secrets
        .iter()
        .map(|entry| entry.id.clone())
        .collect();
    for unknown in values.difference(&declared_values) {
        errors.push(
            format!("ADVANCED_AUDIO_API.values.{unknown}"),
            "is not declared by the workflow",
        );
    }
    for unknown in secrets.difference(&declared_secrets) {
        errors.push(
            format!("ADVANCED_AUDIO_API.secrets.{unknown}"),
            "is not declared by the workflow",
        );
    }
}

fn is_empty_required_value(parameter: &ParameterDefinition, value: &str) -> bool {
    match parameter.parameter_type {
        Some(ParameterType::Text) => value.is_empty(),
        Some(ParameterType::MultiSelect) => parameter
            .parse_value(WorkflowSchemaVersion(CURRENT_SCHEMA_VERSION), value)
            .is_ok_and(|value| matches!(value, Value::Array(ref values) if values.is_empty())),
        Some(
            ParameterType::Integer
            | ParameterType::Number
            | ParameterType::Boolean
            | ParameterType::Select
            | ParameterType::JsonObject
            | ParameterType::JsonArray,
        )
        | None => false,
    }
}

fn validate_remote_audio(
    remote_audio: &RemoteAudioConfig,
    workflow: &AdvancedAudioWorkflow,
    errors: &mut ValidationErrors,
) {
    let delivery = workflow.audio.delivery.kind();
    let needed = matches!(
        delivery,
        AudioDeliveryType::PublicHttpsUrl | AudioDeliveryType::CloudUri
    );
    if !needed {
        return;
    }
    match (remote_audio, delivery) {
        (RemoteAudioConfig::None, _) => errors.push(
            "ADVANCED_AUDIO_API.remote_audio",
            "remote hosting is required by the selected audio delivery",
        ),
        (RemoteAudioConfig::Webdav(settings), AudioDeliveryType::PublicHttpsUrl) => {
            validate_url(
                &settings.upload_base_url,
                "ADVANCED_AUDIO_API.remote_audio.upload_base_url",
                &["http", "https"],
                errors,
            );
            validate_url(
                &settings.public_download_base_url,
                "ADVANCED_AUDIO_API.remote_audio.public_download_base_url",
                &["https"],
                errors,
            );
            required_remote(&settings.username, "username", errors);
            required_remote(&settings.password, "password", errors);
            validate_remote_prefix(
                &settings.remote_path_prefix,
                "ADVANCED_AUDIO_API.remote_audio.remote_path_prefix",
                errors,
            );
        }
        (RemoteAudioConfig::Webdav(_), AudioDeliveryType::CloudUri) => errors.push(
            "ADVANCED_AUDIO_API.remote_audio",
            "WebDAV provides public HTTPS URLs, not cloud URIs",
        ),
        (RemoteAudioConfig::S3Compatible(settings), _) => {
            validate_url(
                &settings.endpoint,
                "ADVANCED_AUDIO_API.remote_audio.endpoint",
                &["http", "https"],
                errors,
            );
            for (value, name) in [
                (&settings.region, "region"),
                (&settings.bucket, "bucket"),
                (&settings.access_key, "access_key"),
                (&settings.secret_key, "secret_key"),
            ] {
                required_remote(value, name, errors);
            }
            if delivery == AudioDeliveryType::PublicHttpsUrl && !settings.presigned {
                match settings.public_url_base.as_deref() {
                    Some(url) => validate_url(
                        url,
                        "ADVANCED_AUDIO_API.remote_audio.public_url_base",
                        &["https"],
                        errors,
                    ),
                    None => errors.push(
                        "ADVANCED_AUDIO_API.remote_audio.public_url_base",
                        "is required when public HTTPS access is not presigned",
                    ),
                }
            }
            if delivery == AudioDeliveryType::PublicHttpsUrl && settings.presigned {
                validate_url(
                    &settings.endpoint,
                    "ADVANCED_AUDIO_API.remote_audio.endpoint",
                    &["https"],
                    errors,
                );
            }
            validate_remote_prefix(
                &settings.prefix,
                "ADVANCED_AUDIO_API.remote_audio.prefix",
                errors,
            );
        }
        (RemoteAudioConfig::AliyunOss(settings), _) => {
            validate_url(
                &settings.endpoint,
                "ADVANCED_AUDIO_API.remote_audio.endpoint",
                &["http", "https"],
                errors,
            );
            for (value, name) in [
                (&settings.bucket, "bucket"),
                (&settings.access_key, "access_key"),
                (&settings.secret_key, "secret_key"),
            ] {
                required_remote(value, name, errors);
            }
            if delivery == AudioDeliveryType::PublicHttpsUrl && !settings.presigned {
                match settings.public_url_base.as_deref() {
                    Some(url) => validate_url(
                        url,
                        "ADVANCED_AUDIO_API.remote_audio.public_url_base",
                        &["https"],
                        errors,
                    ),
                    None => errors.push(
                        "ADVANCED_AUDIO_API.remote_audio.public_url_base",
                        "is required when public HTTPS access is not presigned",
                    ),
                }
            }
            if delivery == AudioDeliveryType::PublicHttpsUrl && settings.presigned {
                validate_url(
                    &settings.endpoint,
                    "ADVANCED_AUDIO_API.remote_audio.endpoint",
                    &["https"],
                    errors,
                );
            }
            validate_remote_prefix(
                &settings.prefix,
                "ADVANCED_AUDIO_API.remote_audio.prefix",
                errors,
            );
        }
        _ => {}
    }
}

fn required_remote(value: &str, name: &str, errors: &mut ValidationErrors) {
    if value.trim().is_empty() {
        errors.push(
            format!("ADVANCED_AUDIO_API.remote_audio.{name}"),
            "must not be empty",
        );
    }
}

fn validate_remote_prefix(value: &str, path: &str, errors: &mut ValidationErrors) {
    if value.len() > 1_024 || value.contains('\0') {
        errors.push(path, "must contain at most 1024 bytes and no NUL bytes");
    }
    if value.split('/').any(|segment| segment == "..") {
        errors.push(path, "must not contain parent path segments");
    }
}

fn validate_non_realtime_audio(audio: &AudioSpec, path: &str, errors: &mut ValidationErrors) {
    if matches!(audio.delivery, AudioDelivery::RealtimeChunks) {
        errors.push(path, "realtime_chunks is only valid for realtime_session");
    }
    if let Some(mime) = &audio.mime {
        validate_text(mime, &format!("{path}.mime"), true, 256, errors);
    }
}

/// Audio delivery is a protocol choice, not merely metadata.  At least one
/// pre-recognition stage must actually carry the declared representation.
/// This catches hand-written workflows that would otherwise validate while
/// silently sending no audio at all.
fn validate_audio_delivery_usage(
    audio: &AudioSpec,
    stages: &[&HttpStage],
    path: &str,
    errors: &mut ValidationErrors,
) {
    let used = match audio.delivery {
        AudioDelivery::MultipartFile => stages.iter().any(|stage| stage_has_audio_file(stage)),
        AudioDelivery::RawAudio => stages.iter().any(|stage| stage_has_raw_audio(stage)),
        AudioDelivery::Base64 => stages
            .iter()
            .any(|stage| stage_references_audio(stage, Some(AudioPlaceholder::Base64))),
        AudioDelivery::DataUri => stages
            .iter()
            .any(|stage| stage_references_audio(stage, Some(AudioPlaceholder::DataUri))),
        AudioDelivery::PublicHttpsUrl => stages
            .iter()
            .any(|stage| stage_references_audio(stage, Some(AudioPlaceholder::PublicUrl))),
        AudioDelivery::CloudUri => stages
            .iter()
            .any(|stage| stage_references_audio(stage, Some(AudioPlaceholder::CloudUri))),
        AudioDelivery::ProviderUpload | AudioDelivery::RealtimeChunks => true,
    };
    if !used {
        errors.push(
            path,
            format!(
                "{} must be used by a pre-recognition HTTP stage",
                audio_delivery_name(audio.delivery.kind())
            ),
        );
    }
}

fn validate_provider_upload(
    prepare: Option<&HttpStage>,
    submit: Option<&HttpStage>,
    audio: &AudioSpec,
    errors: &mut ValidationErrors,
) {
    if !matches!(audio.delivery, AudioDelivery::ProviderUpload) {
        return;
    }
    let Some(prepare) = prepare else {
        errors.push(
            "audio.delivery",
            "provider_upload requires an async_poll prepare upload stage",
        );
        return;
    };
    let Some(submit) = submit else {
        errors.push(
            "audio.delivery",
            "provider_upload requires an async_poll submit stage",
        );
        return;
    };
    if !stage_transfers_local_audio(prepare) {
        errors.push(
            "recognition.prepare.body",
            "provider_upload prepare stage must upload typed multipart audio or raw_audio",
        );
    }
    if prepare.captures.is_empty() {
        errors.push(
            "recognition.prepare.captures",
            "provider_upload prepare stage must capture its provider audio reference",
        );
        return;
    }
    let prepare_capture_ids = prepare
        .captures
        .iter()
        .map(|capture| capture.id.as_str())
        .collect::<BTreeSet<_>>();
    if !stage_references_capture_ids(submit, &prepare_capture_ids) {
        errors.push(
            "recognition.submit",
            "provider_upload submit stage must reference a capture from prepare",
        );
    }
}

fn audio_delivery_name(delivery: AudioDeliveryType) -> &'static str {
    match delivery {
        AudioDeliveryType::MultipartFile => "multipart_file delivery",
        AudioDeliveryType::RawAudio => "raw_audio delivery",
        AudioDeliveryType::Base64 => "base64 delivery",
        AudioDeliveryType::DataUri => "data_uri delivery",
        AudioDeliveryType::PublicHttpsUrl => "public_https_url delivery",
        AudioDeliveryType::CloudUri => "cloud_uri delivery",
        AudioDeliveryType::ProviderUpload => "provider_upload delivery",
        AudioDeliveryType::RealtimeChunks => "realtime_chunks delivery",
    }
}

fn stage_has_audio_file(stage: &HttpStage) -> bool {
    matches!(
        &stage.body,
        HttpBody::Multipart { fields }
            if fields.iter().any(|field| matches!(field.value, MultipartValue::AudioFile))
    )
}

fn stage_has_raw_audio(stage: &HttpStage) -> bool {
    matches!(stage.body, HttpBody::RawAudio)
}

fn stage_transfers_local_audio(stage: &HttpStage) -> bool {
    stage_has_audio_file(stage) || stage_has_raw_audio(stage)
}

fn stage_references_audio(stage: &HttpStage, wanted: Option<AudioPlaceholder>) -> bool {
    template_references_audio(&stage.url, wanted)
        || stage
            .query
            .values()
            .any(|value| template_references_audio(value, wanted))
        || stage
            .headers
            .values()
            .any(|value| template_references_audio(value, wanted))
        || body_references_audio(&stage.body, wanted)
}

fn body_references_audio(body: &HttpBody, wanted: Option<AudioPlaceholder>) -> bool {
    match body {
        HttpBody::None | HttpBody::RawAudio => false,
        HttpBody::RawBytes { value } => template_references_audio(value, wanted),
        HttpBody::FormUrlencoded { fields } => fields
            .values()
            .any(|value| template_references_audio(value, wanted)),
        HttpBody::Multipart { fields } => fields.iter().any(|field| match &field.value {
            MultipartValue::Text { value } | MultipartValue::Bytes { value } => {
                template_references_audio(value, wanted)
            }
            MultipartValue::AudioFile => false,
        }),
        HttpBody::Json { value } => json_references_audio(value, wanted),
    }
}

fn json_references_audio(value: &Value, wanted: Option<AudioPlaceholder>) -> bool {
    match value {
        Value::String(value) => template_references_audio(value, wanted),
        Value::Array(values) => values
            .iter()
            .any(|value| json_references_audio(value, wanted)),
        Value::Object(values) => values
            .values()
            .any(|value| json_references_audio(value, wanted)),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

fn template_references_audio(value: &str, wanted: Option<AudioPlaceholder>) -> bool {
    Template::parse(value).is_ok_and(|template| {
        template.placeholders().any(|placeholder| {
            matches!(placeholder, Placeholder::Audio(audio) if wanted.is_none_or(|wanted| wanted == *audio))
        })
    })
}

fn stage_references_capture_ids(stage: &HttpStage, wanted: &BTreeSet<&str>) -> bool {
    template_references_capture_ids(&stage.url, wanted)
        || stage
            .query
            .values()
            .any(|value| template_references_capture_ids(value, wanted))
        || stage
            .headers
            .values()
            .any(|value| template_references_capture_ids(value, wanted))
        || body_references_capture_ids(&stage.body, wanted)
}

fn body_references_capture_ids(body: &HttpBody, wanted: &BTreeSet<&str>) -> bool {
    match body {
        HttpBody::None | HttpBody::RawAudio => false,
        HttpBody::RawBytes { value } => template_references_capture_ids(value, wanted),
        HttpBody::FormUrlencoded { fields } => fields
            .values()
            .any(|value| template_references_capture_ids(value, wanted)),
        HttpBody::Multipart { fields } => fields.iter().any(|field| match &field.value {
            MultipartValue::Text { value } | MultipartValue::Bytes { value } => {
                template_references_capture_ids(value, wanted)
            }
            MultipartValue::AudioFile => false,
        }),
        HttpBody::Json { value } => json_references_capture_ids(value, wanted),
    }
}

fn json_references_capture_ids(value: &Value, wanted: &BTreeSet<&str>) -> bool {
    match value {
        Value::String(value) => template_references_capture_ids(value, wanted),
        Value::Array(values) => values
            .iter()
            .any(|value| json_references_capture_ids(value, wanted)),
        Value::Object(values) => values
            .values()
            .any(|value| json_references_capture_ids(value, wanted)),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

fn template_references_capture_ids(value: &str, wanted: &BTreeSet<&str>) -> bool {
    Template::parse(value).is_ok_and(|template| {
        template.placeholders().any(|placeholder| {
            matches!(placeholder, Placeholder::Capture(capture) if wanted.contains(capture.as_str()))
        })
    })
}

struct TemplateScope<'a> {
    parameters: &'a BTreeSet<String>,
    secrets: &'a BTreeSet<String>,
    captures: &'a BTreeSet<String>,
    audio_delivery: AudioDeliveryType,
    realtime: bool,
    audio_access: AudioTemplateAccess,
}

/// The same workflow has different audio-template contexts.  A WebSocket
/// connect or finish message has no prepared-audio metadata at runtime; only
/// the per-chunk message receives an audio value.
#[derive(Clone, Copy)]
enum AudioTemplateAccess {
    Delivery,
    None,
    RealtimeChunkOnly,
}

impl<'a> TemplateScope<'a> {
    fn new(
        parameters: &'a BTreeSet<String>,
        secrets: &'a BTreeSet<String>,
        captures: &'a BTreeSet<String>,
        audio_delivery: AudioDeliveryType,
        realtime: bool,
    ) -> Self {
        Self {
            parameters,
            secrets,
            captures,
            audio_delivery,
            realtime,
            audio_access: AudioTemplateAccess::Delivery,
        }
    }

    fn with_captures<'b>(&'b self, captures: &'b BTreeSet<String>) -> TemplateScope<'b> {
        TemplateScope::new(
            self.parameters,
            self.secrets,
            captures,
            self.audio_delivery,
            self.realtime,
        )
    }

    fn realtime(&self) -> Self {
        Self::new(
            self.parameters,
            self.secrets,
            self.captures,
            self.audio_delivery,
            true,
        )
    }

    fn without_audio(&self) -> Self {
        Self {
            parameters: self.parameters,
            secrets: self.secrets,
            captures: self.captures,
            audio_delivery: self.audio_delivery,
            realtime: self.realtime,
            audio_access: AudioTemplateAccess::None,
        }
    }

    fn realtime_chunk_only(&self) -> Self {
        Self {
            parameters: self.parameters,
            secrets: self.secrets,
            captures: self.captures,
            audio_delivery: self.audio_delivery,
            realtime: true,
            audio_access: AudioTemplateAccess::RealtimeChunkOnly,
        }
    }
}

fn validate_http_stage(
    stage: &HttpStage,
    path: &str,
    base_scope: &TemplateScope<'_>,
    captures: &mut BTreeSet<String>,
    errors: &mut ValidationErrors,
) {
    let scope = base_scope.with_captures(captures);
    validate_template_string(&stage.url, &format!("{path}.url"), &scope, errors);
    validate_http_url_template(&stage.url, &format!("{path}.url"), errors);
    validate_pairs(
        &stage.query,
        &format!("{path}.query"),
        &scope,
        MAX_QUERY,
        errors,
    );
    validate_pairs(
        &stage.headers,
        &format!("{path}.headers"),
        &scope,
        MAX_HEADERS,
        errors,
    );
    if stage.accepted_statuses.is_empty() || stage.accepted_statuses.len() > 32 {
        errors.push(
            format!("{path}.accepted_statuses"),
            "must contain 1..=32 HTTP statuses",
        );
    }
    for (index, status) in stage.accepted_statuses.iter().enumerate() {
        if !(100..=599).contains(status) {
            errors.push(
                format!("{path}.accepted_statuses[{index}]"),
                "must be in HTTP range 100..=599",
            );
        }
    }
    validate_signer(
        &stage.signer,
        &format!("{path}.signer"),
        base_scope.secrets,
        errors,
    );
    validate_http_body(&stage.body, &format!("{path}.body"), &scope, errors);
    if !matches!(stage.signer, SignerConfig::None)
        && matches!(stage.body, HttpBody::Multipart { .. } | HttpBody::RawAudio)
    {
        // These bodies are deliberately streamed from disk.  Dynamic signers
        // hash the exact request payload, and the engine correctly refuses to
        // materialize them.  Reject the combination here rather than letting
        // a saved, otherwise-valid workflow fail only at execution time.
        errors.push(
            format!("{path}.signer"),
            "cannot be used with multipart or raw_audio bodies because those uploads remain streaming",
        );
    }
    if stage.captures.len() > MAX_CAPTURES {
        errors.push(
            format!("{path}.captures"),
            format!("must contain at most {MAX_CAPTURES} captures"),
        );
    }
    let mut local = BTreeSet::new();
    for (index, capture) in stage.captures.iter().enumerate() {
        let capture_path = format!("{path}.captures[{index}]");
        if !is_identifier(&capture.id) {
            errors.push(format!("{capture_path}.id"), "must be an ASCII identifier");
        } else if captures.contains(&capture.id) || !local.insert(capture.id.clone()) {
            errors.push(
                format!("{capture_path}.id"),
                format!("capture '{}' is already defined", capture.id),
            );
        }
        validate_extractor(&capture.from, &format!("{capture_path}.from"), errors);
    }
    captures.extend(local);
}

fn validate_http_url_template(value: &str, path: &str, errors: &mut ValidationErrors) {
    // A service can return a documented, presigned result/download URL after
    // an earlier stage. The complete capture is validated again after
    // rendering and before a network request is created.
    if capture_only_http_url_template_id(value).is_some() {
        return;
    }
    validate_url_template(value, path, &["http", "https"], errors);
}

/// Returns the capture ID only when the template is exactly one capture.
///
/// HTTP-stage validation uses this to allow a documented result URL from an
/// earlier response. Runtime uses the same predicate to classify the capture
/// as sensitive: a complete dynamic URL can carry a short-lived credential in
/// its query string.
pub(crate) fn capture_only_http_url_template_id(value: &str) -> Option<&str> {
    value
        .strip_prefix("{{capture:")
        .and_then(|name| name.strip_suffix("}}"))
        .filter(|name| is_identifier(name))
}

fn validate_url_template(value: &str, path: &str, schemes: &[&str], errors: &mut ValidationErrors) {
    // Template URLs cannot be fully parsed before rendering, but their fixed
    // prefix must still name an allowed network scheme.  This prevents file:
    // or arbitrary local paths from entering the runtime.
    let prefix = value.split("{{").next().unwrap_or(value);
    let valid = schemes.iter().any(|scheme| {
        prefix.eq_ignore_ascii_case(&format!("{scheme}://"))
            || prefix.starts_with(&format!("{scheme}://"))
    });
    if !valid {
        errors.push(
            path,
            format!("must start with one of {}://", schemes.join(", ")),
        );
    }
}

fn validate_pairs(
    pairs: &BTreeMap<String, String>,
    path: &str,
    scope: &TemplateScope<'_>,
    maximum: usize,
    errors: &mut ValidationErrors,
) {
    if pairs.len() > maximum {
        errors.push(path, format!("must contain at most {maximum} entries"));
    }
    for (name, value) in pairs {
        validate_text(name, &format!("{path}.{name}"), true, 256, errors);
        validate_template_string(value, &format!("{path}.{name}"), scope, errors);
    }
}

fn validate_http_body(
    body: &HttpBody,
    path: &str,
    scope: &TemplateScope<'_>,
    errors: &mut ValidationErrors,
) {
    match body {
        HttpBody::None => {}
        HttpBody::RawAudio => {
            if !matches!(
                scope.audio_delivery,
                AudioDeliveryType::RawAudio | AudioDeliveryType::ProviderUpload
            ) {
                errors.push(
                    path,
                    "raw_audio body is incompatible with the selected audio delivery",
                );
            }
        }
        HttpBody::RawBytes { value } => {
            validate_template_string(value, &format!("{path}.value"), scope, errors)
        }
        HttpBody::FormUrlencoded { fields } => {
            validate_pairs(fields, &format!("{path}.fields"), scope, MAX_QUERY, errors)
        }
        HttpBody::Json { value } => {
            validate_json_templates(value, &format!("{path}.value"), scope, errors)
        }
        HttpBody::Multipart { fields } => {
            if fields.is_empty() || fields.len() > MAX_MULTIPART_FIELDS {
                errors.push(
                    path,
                    format!("must contain 1..={MAX_MULTIPART_FIELDS} fields"),
                );
            }
            let mut names = BTreeSet::new();
            let mut audio_fields = 0;
            for (index, field) in fields.iter().enumerate() {
                let field_path = format!("{path}.fields[{index}]");
                validate_text(
                    &field.name,
                    &format!("{field_path}.name"),
                    true,
                    256,
                    errors,
                );
                if !names.insert(field.name.clone()) {
                    errors.push(format!("{field_path}.name"), "duplicate multipart field");
                }
                match &field.value {
                    MultipartValue::Text { value } | MultipartValue::Bytes { value } => {
                        validate_template_string(
                            value,
                            &format!("{field_path}.value"),
                            scope,
                            errors,
                        )
                    }
                    MultipartValue::AudioFile => audio_fields += 1,
                }
            }
            if audio_fields > 1 {
                errors.push(path, "may contain at most one typed audio_file field");
            }
            if audio_fields > 0
                && !matches!(
                    scope.audio_delivery,
                    AudioDeliveryType::MultipartFile | AudioDeliveryType::ProviderUpload
                )
            {
                errors.push(
                    path,
                    "typed multipart audio_file is incompatible with the selected audio delivery",
                );
            }
        }
    }
}

fn validate_json_templates(
    value: &Value,
    path: &str,
    scope: &TemplateScope<'_>,
    errors: &mut ValidationErrors,
) {
    match value {
        Value::String(value) => validate_template_string(value, path, scope, errors),
        Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                validate_json_templates(value, &format!("{path}[{index}]"), scope, errors);
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                validate_json_templates(value, &format!("{path}.{key}"), scope, errors);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn validate_template_string(
    value: &str,
    path: &str,
    scope: &TemplateScope<'_>,
    errors: &mut ValidationErrors,
) {
    if value.len() > MAX_TEMPLATE_BYTES {
        errors.push(path, format!("template exceeds {MAX_TEMPLATE_BYTES} bytes"));
        return;
    }
    let template = match Template::parse(value) {
        Ok(template) => template,
        Err(error) => {
            errors.push(path, error.to_string());
            return;
        }
    };
    for placeholder in template.placeholders() {
        match placeholder {
            Placeholder::Var(name) if !scope.parameters.contains(name) => {
                errors.push(path, format!("references undeclared variable '{name}'"))
            }
            Placeholder::Secret(name) if !scope.secrets.contains(name) => {
                errors.push(path, format!("references undeclared secret '{name}'"))
            }
            Placeholder::Capture(name) if !scope.captures.contains(name) => errors.push(
                path,
                format!("references capture '{name}' before it is created"),
            ),
            Placeholder::Audio(audio) if !audio_placeholder_allowed(*audio, scope) => errors.push(
                path,
                format!("{placeholder} is incompatible with this audio delivery"),
            ),
            Placeholder::Var(_)
            | Placeholder::Secret(_)
            | Placeholder::Capture(_)
            | Placeholder::Runtime(_)
            | Placeholder::Audio(_) => {}
        }
    }
}

fn audio_placeholder_allowed(audio: AudioPlaceholder, scope: &TemplateScope<'_>) -> bool {
    match scope.audio_access {
        AudioTemplateAccess::None => return false,
        AudioTemplateAccess::RealtimeChunkOnly => {
            return audio == AudioPlaceholder::ChunkBase64;
        }
        AudioTemplateAccess::Delivery => {}
    }
    match audio {
        AudioPlaceholder::Filename | AudioPlaceholder::Mime | AudioPlaceholder::Size => true,
        AudioPlaceholder::Base64 => matches!(scope.audio_delivery, AudioDeliveryType::Base64),
        AudioPlaceholder::DataUri => scope.audio_delivery == AudioDeliveryType::DataUri,
        AudioPlaceholder::PublicUrl => scope.audio_delivery == AudioDeliveryType::PublicHttpsUrl,
        AudioPlaceholder::CloudUri => scope.audio_delivery == AudioDeliveryType::CloudUri,
        AudioPlaceholder::ChunkBase64 => {
            scope.realtime && scope.audio_delivery == AudioDeliveryType::RealtimeChunks
        }
    }
}

fn validate_extractor(extractor: &ResponseExtractor, path: &str, errors: &mut ValidationErrors) {
    match extractor {
        ResponseExtractor::JsonPath { path: json_path } => {
            if let Err(error) = crate::jsonpath::parse_text_path(json_path) {
                errors.push(path, error.to_string());
            }
        }
        ResponseExtractor::Header { name } => {
            if name.trim().is_empty() || name.len() > 256 || name.contains(['\r', '\n']) {
                errors.push(path, "header name must be a non-empty HTTP header name");
            }
        }
        ResponseExtractor::PlainBody | ResponseExtractor::Status => {}
    }
}

fn validate_stream(
    stream: &StreamResponse,
    path: &str,
    scope: &TemplateScope<'_>,
    require_complete: bool,
    errors: &mut ValidationErrors,
) {
    if stream.rules.is_empty() || stream.rules.len() > MAX_STREAM_RULES {
        errors.push(
            format!("{path}.rules"),
            format!("must contain 1..={MAX_STREAM_RULES} rules"),
        );
    }
    let mut has_complete = false;
    for (index, rule) in stream.rules.iter().enumerate() {
        validate_stream_rule(rule, &format!("{path}.rules[{index}]"), scope, errors);
        has_complete |= rule.action == StreamAction::Complete;
    }
    if require_complete && !has_complete {
        errors.push(format!("{path}.rules"), "must include a complete action");
    }
}

fn validate_stream_rule(
    rule: &StreamRule,
    path: &str,
    _scope: &TemplateScope<'_>,
    errors: &mut ValidationErrors,
) {
    if let Some(event) = &rule.event {
        validate_text(event, &format!("{path}.event"), true, 256, errors);
    }
    if let Some(path_value) = &rule.path
        && let Err(error) = crate::jsonpath::parse_text_path(path_value)
    {
        errors.push(format!("{path}.path"), error.to_string());
    }
    if matches!(
        rule.action,
        StreamAction::AppendDelta
            | StreamAction::ReplacePartial
            | StreamAction::CommitSegment
            | StreamAction::SetFinalText
            | StreamAction::Fail
    ) && rule.path.is_none()
    {
        errors.push(
            format!("{path}.path"),
            "is required for this transcript action",
        );
    }
    if rule.equals.is_some() && rule.path.is_none() {
        errors.push(
            format!("{path}.equals"),
            "requires path so a value can be compared",
        );
    }
}

fn validate_poll_stage(
    stage: &PollStage,
    path: &str,
    base_scope: &TemplateScope<'_>,
    captures: &mut BTreeSet<String>,
    errors: &mut ValidationErrors,
) {
    if !matches!(stage.request.method, HttpMethod::Get | HttpMethod::Post) {
        errors.push(
            format!("{path}.request.method"),
            "poll requests support only GET or POST",
        );
    }
    if stage.interval_ms < MIN_POLL_INTERVAL_MS {
        errors.push(
            format!("{path}.interval_ms"),
            format!("must be at least {MIN_POLL_INTERVAL_MS}"),
        );
    }
    if stage.timeout_ms < stage.interval_ms || stage.timeout_ms > MAX_POLL_TIMEOUT_MS {
        errors.push(
            format!("{path}.timeout_ms"),
            format!("must be between interval_ms and {MAX_POLL_TIMEOUT_MS}"),
        );
    }
    validate_http_stage(
        &stage.request,
        &format!("{path}.request"),
        base_scope,
        captures,
        errors,
    );
    if stage_transfers_local_audio(&stage.request) || stage_references_audio(&stage.request, None) {
        errors.push(
            format!("{path}.request"),
            "poll requests must not resend audio or an audio reference",
        );
    }
    validate_condition_group(&stage.pending, &format!("{path}.pending"), errors);
    validate_condition_group(&stage.success, &format!("{path}.success"), errors);
    validate_condition_group(&stage.failure, &format!("{path}.failure"), errors);
    if stage.pending.is_empty() || stage.success.is_empty() || stage.failure.is_empty() {
        errors.push(
            path,
            "must declare at least one pending, success, and failure condition",
        );
    }
}

fn validate_condition_group(
    conditions: &[PollCondition],
    path: &str,
    errors: &mut ValidationErrors,
) {
    if conditions.len() > MAX_STREAM_RULES {
        errors.push(
            path,
            format!("must contain at most {MAX_STREAM_RULES} conditions"),
        );
    }
    for (index, condition) in conditions.iter().enumerate() {
        let condition_path = format!("{path}[{index}]");
        validate_extractor(&condition.from, &format!("{condition_path}.from"), errors);
        match condition.operator {
            PollOperator::Eq | PollOperator::Ne => {
                if condition.value.is_none() {
                    errors.push(format!("{condition_path}.value"), "is required for eq/ne");
                }
            }
            PollOperator::In => {
                if condition.values.is_empty() {
                    errors.push(format!("{condition_path}.values"), "is required for in");
                }
            }
            PollOperator::Exists
            | PollOperator::NotExists
            | PollOperator::IsTrue
            | PollOperator::IsFalse => {
                if condition.value.is_some() || !condition.values.is_empty() {
                    errors.push(
                        &condition_path,
                        "does not accept value(s) for this operator",
                    );
                }
            }
        }
    }
}

fn validate_signer(
    signer: &SignerConfig,
    path: &str,
    declared_secrets: &BTreeSet<String>,
    errors: &mut ValidationErrors,
) {
    let check = |id: &str, key: &str, errors: &mut ValidationErrors| {
        if !declared_secrets.contains(id) {
            errors.push(
                format!("{path}.{key}"),
                format!("references undeclared secret '{id}'"),
            );
        }
    };
    match signer {
        SignerConfig::None => {}
        SignerConfig::AwsSigv4 {
            region,
            service,
            access_key_secret,
            secret_key_secret,
            session_token_secret,
        } => {
            validate_text(region, &format!("{path}.region"), true, 256, errors);
            validate_text(service, &format!("{path}.service"), true, 256, errors);
            check(access_key_secret, "access_key_secret", errors);
            check(secret_key_secret, "secret_key_secret", errors);
            if let Some(id) = session_token_secret {
                check(id, "session_token_secret", errors);
            }
        }
        SignerConfig::TencentTc3 {
            service,
            secret_id_secret,
            secret_key_secret,
        } => {
            validate_text(service, &format!("{path}.service"), true, 256, errors);
            check(secret_id_secret, "secret_id_secret", errors);
            check(secret_key_secret, "secret_key_secret", errors);
        }
    }
}

fn validate_realtime(
    realtime: &RealtimeWorkflow,
    path: &str,
    base_scope: &TemplateScope<'_>,
    errors: &mut ValidationErrors,
) {
    if realtime.transport != RealtimeTransport::WebSocket {
        errors.push(
            format!("{path}.transport"),
            "Unsupported transport; this build implements only websocket",
        );
    }
    // Connection setup and ordered control messages render before any audio
    // chunk exists.  Do not let a draft pass validation only to fail with a
    // missing audio value during WebSocket setup/finalization.
    let connection_scope = base_scope.realtime().without_audio();
    let audio_message_scope = base_scope.realtime_chunk_only();
    validate_template_string(
        &realtime.connect.url,
        &format!("{path}.connect.url"),
        &connection_scope,
        errors,
    );
    validate_url_template(
        &realtime.connect.url,
        &format!("{path}.connect.url"),
        &["ws", "wss"],
        errors,
    );
    validate_pairs(
        &realtime.connect.query,
        &format!("{path}.connect.query"),
        &connection_scope,
        MAX_QUERY,
        errors,
    );
    validate_pairs(
        &realtime.connect.headers,
        &format!("{path}.connect.headers"),
        &connection_scope,
        MAX_HEADERS,
        errors,
    );
    validate_signer(
        &realtime.connect.signer,
        &format!("{path}.connect.signer"),
        connection_scope.secrets,
        errors,
    );
    if let Some(protocol) = &realtime.connect.subprotocol {
        validate_text(
            protocol,
            &format!("{path}.connect.subprotocol"),
            true,
            256,
            errors,
        );
        validate_template_string(
            protocol,
            &format!("{path}.connect.subprotocol"),
            &connection_scope,
            errors,
        );
    }
    validate_messages(
        &realtime.initial_messages,
        &format!("{path}.initial_messages"),
        &connection_scope,
        errors,
    );
    validate_messages(
        &realtime.finish_messages,
        &format!("{path}.finish_messages"),
        &connection_scope,
        errors,
    );
    validate_realtime_audio(realtime, path, errors);
    match &realtime.audio_message {
        RealtimeAudioMessage::Binary => {}
        RealtimeAudioMessage::Text { value } => {
            let value_path = format!("{path}.audio_message.value");
            validate_template_string(value, &value_path, &audio_message_scope, errors);
            if !contains_chunk_placeholder(value) {
                errors.push(
                    value_path,
                    "must reference {{audio:chunk_base64}} for each audio chunk",
                );
            }
        }
        RealtimeAudioMessage::Json { value } => {
            let value_path = format!("{path}.audio_message.value");
            validate_json_templates(value, &value_path, &audio_message_scope, errors);
            if !json_contains_chunk_placeholder(value) {
                errors.push(
                    value_path,
                    "must reference {{audio:chunk_base64}} for each audio chunk",
                );
            }
        }
    }
    let stream = StreamResponse {
        format: StreamFormat::JsonChunks,
        rules: realtime.receive_rules.clone(),
    };
    validate_stream(
        &stream,
        &format!("{path}.receive"),
        &connection_scope,
        !realtime_completion_can_complete(&realtime.completion),
        errors,
    );
    validate_realtime_completion(&realtime.completion, &format!("{path}.completion"), errors);
    if realtime.pause_behavior != PauseBehavior::RestartSession {
        errors.push(
            format!("{path}.pause_behavior"),
            "Unsupported pause behavior; this build implements only restart_session",
        );
    }
    if realtime.finalization_timeout_ms == 0
        || realtime.finalization_timeout_ms > MAX_POLL_TIMEOUT_MS
    {
        errors.push(
            format!("{path}.finalization_timeout_ms"),
            "must be in 1..=86400000",
        );
    }
}

fn realtime_completion_can_complete(completion: &RealtimeCompletion) -> bool {
    completion.event.is_some() || completion.path.is_some()
}

fn contains_chunk_placeholder(value: &str) -> bool {
    Template::parse(value).is_ok_and(|template| {
        template.placeholders().any(|placeholder| {
            matches!(
                placeholder,
                Placeholder::Audio(AudioPlaceholder::ChunkBase64)
            )
        })
    })
}

fn json_contains_chunk_placeholder(value: &Value) -> bool {
    match value {
        Value::String(value) => contains_chunk_placeholder(value),
        Value::Array(values) => values.iter().any(json_contains_chunk_placeholder),
        Value::Object(values) => values.values().any(json_contains_chunk_placeholder),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

fn validate_realtime_completion(
    completion: &RealtimeCompletion,
    path: &str,
    errors: &mut ValidationErrors,
) {
    if let Some(event) = &completion.event {
        validate_text(event, &format!("{path}.event"), true, 256, errors);
    }
    if let Some(json_path) = &completion.path
        && let Err(error) = crate::jsonpath::parse_text_path(json_path)
    {
        errors.push(format!("{path}.path"), error.to_string());
    }
    if completion.equals.is_some() && completion.path.is_none() {
        errors.push(
            format!("{path}.equals"),
            "requires completion.path so a value can be compared",
        );
    }
}

fn validate_messages(
    messages: &[RealtimeMessage],
    path: &str,
    scope: &TemplateScope<'_>,
    errors: &mut ValidationErrors,
) {
    if messages.len() > MAX_MESSAGES {
        errors.push(
            path,
            format!("must contain at most {MAX_MESSAGES} messages"),
        );
    }
    for (index, message) in messages.iter().enumerate() {
        let item_path = format!("{path}[{index}]");
        match message {
            RealtimeMessage::Json { value } => {
                validate_json_templates(value, &item_path, scope, errors)
            }
            RealtimeMessage::Text { value } | RealtimeMessage::Binary { value } => {
                validate_template_string(value, &item_path, scope, errors)
            }
        }
    }
}

fn validate_realtime_audio(realtime: &RealtimeWorkflow, path: &str, errors: &mut ValidationErrors) {
    let audio = &realtime.audio_stream;
    validate_text(
        &audio.codec,
        &format!("{path}.audio_stream.codec"),
        true,
        128,
        errors,
    );
    if !audio.codec.eq_ignore_ascii_case("pcm_s16le") {
        errors.push(
            format!("{path}.audio_stream.codec"),
            "Unsupported codec; this build implements only pcm_s16le realtime audio",
        );
    }
    if audio.sample_rate == 0 || audio.sample_rate > 384_000 {
        errors.push(
            format!("{path}.audio_stream.sample_rate"),
            "must be in 1..=384000",
        );
    }
    if audio.channels == 0 || audio.channels > 8 {
        errors.push(format!("{path}.audio_stream.channels"), "must be in 1..=8");
    }
    if !(MIN_CHUNK_DURATION_MS..=MAX_CHUNK_DURATION_MS).contains(&audio.chunk_duration_ms) {
        errors.push(
            format!("{path}.audio_stream.chunk_duration_ms"),
            format!("must be in {MIN_CHUNK_DURATION_MS}..={MAX_CHUNK_DURATION_MS}"),
        );
    }
    if audio.pacing != RealtimePacing::Realtime {
        errors.push(
            format!("{path}.audio_stream.pacing"),
            "Unsupported pacing; this build implements only realtime",
        );
    }
}

fn validate_text(
    value: &str,
    path: &str,
    required: bool,
    maximum: usize,
    errors: &mut ValidationErrors,
) {
    if required && value.trim().is_empty() {
        errors.push(path, "must not be empty");
    }
    if value.len() > maximum {
        errors.push(path, format!("must contain at most {maximum} bytes"));
    }
    if value.contains(['\r', '\n', '\0']) {
        errors.push(path, "must not contain line breaks or NUL bytes");
    }
}

fn validate_url(value: &str, path: &str, schemes: &[&str], errors: &mut ValidationErrors) {
    match reqwest::Url::parse(value) {
        Ok(url)
            if schemes
                .iter()
                .any(|scheme| url.scheme().eq_ignore_ascii_case(scheme))
                && url.host_str().is_some() => {}
        _ => errors.push(
            path,
            format!("must be an absolute {} URL", schemes.join(" or ")),
        ),
    }
}

/// A stable, Core-owned description injected into the GUI's compiler prompt.
/// GUI code must not maintain a second handwritten workflow schema.
pub fn workflow_schema_description() -> String {
    let description = concat!(
        "Root fields: schema_version:number (exact version 2), name:string, parameters:ParameterDefinition[], secrets:SecretDefinition[], audio:AudioSpec, recognition:Recognition. ParameterDefinition is id, label, required:boolean, type:text|integer|number|boolean|select|multi_select|json_object|json_array, optional default:string literal, optional description:string, optional options:ParameterOption[], optional visible_when:VisibilityCondition. ParameterOption is value:string plus label:string. select and multi_select require nonempty unique options; their defaults must use declared option values. All parameter defaults and saved values are strings. A multi_select default or saved value is a string whose contents are a JSON string array of unique option values, for example \"[\\\"zh\\\",\\\"en\\\"]\"; never emit an actual JSON array for its default. json_object and json_array defaults or saved values are strings whose contents are respectively a JSON object or JSON array; never emit a raw JSON object or array for their defaults. integer/number/boolean defaults are their JSON textual forms, such as 16000, 0.5, true, or false, stored as strings. VisibilityCondition is parameter:string plus exactly one of equals:string or nonempty one_of:string[]; it may reference only an earlier unconditional boolean/select parameter with a default, and is GUI presentation metadata only: it creates no request branch and never relaxes required values. SecretDefinition is id, label, required:boolean, optional description:string; IDs are ASCII identifiers. Generate schema version 2 only; this build separately retains schema version 1 text-only workflows for existing configurations.\n",
        "AudioSpec is delivery:AudioDelivery plus optional mime:string. Delivery type is multipart_file|raw_audio|base64|data_uri|public_https_url|cloud_uri|provider_upload|realtime_chunks.\n",
        "Recognition is tagged by mode: request(request:HttpStage, final_text:ResponseExtractor); request_stream(request:HttpStage, stream:StreamResponse); async_poll(optional prepare:HttpStage, submit:HttpStage, optional poll:PollStage, result_steps:HttpStage[] at most 2, final_text:ResponseExtractor); realtime_session(realtime:RealtimeWorkflow). No arbitrary steps, callbacks, branches, or loops exist; only poll repeats.\n",
        "HttpStage has method GET|POST|PUT|PATCH|DELETE, url, optional query:string map, headers:string map, body:HttpBody, accepted_statuses:number[] default [200], signer:SignerConfig, captures:Capture[]. A literal HttpStage URL, including async_poll result_steps, must start with http:// or https:// before any template. When an earlier response documents a complete absolute HTTP(S) URL, a later HttpStage URL may be exactly {{capture:id}}; its rendered value is checked for an absolute HTTP(S) URL before the request. Relative paths and other template-only URLs are invalid. HttpBody is tagged none; json(value:any JSON); form_urlencoded(fields:string map); multipart(fields of name plus text(value), bytes(base64 value), or audio_file); raw_audio; raw_bytes(value). Typed audio_file is the only multipart binary audio field. Multipart/raw_audio remain streaming and cannot use a dynamic signer.\n",
        "ResponseExtractor is tagged json_path(path), header(name), plain_body, or status. Capture is id, from:ResponseExtractor, optional sensitive:boolean; only a later stage may reference it. A capture used as an entire dynamic HTTP URL is automatically treated as sensitive for redaction.\n",
        "StreamResponse has format sse|ndjson|json_chunks and rules:StreamRule[]. StreamRule has optional event, optional JSONPath path, action ignore|append_delta|replace_partial|commit_segment|set_final_text|complete|fail, optional equals. Text actions require path; equals requires path including conditional complete; one complete action is required.\n",
        "PollStage has request, interval_ms, timeout_ms, pending/success/failure PollCondition arrays. Poll uses GET or POST only and cannot carry/refer to audio. PollCondition has from, operator eq|ne|exists|not_exists|is_true|is_false|in, value for eq/ne or values for in; each condition array is nonempty.\n",
        "SignerConfig is tagged none; aws_sigv4(region, service, access_key_secret, secret_key_secret, optional session_token_secret); or tencent_tc3(service, secret_id_secret, secret_key_secret). Referenced secret IDs must be declared.\n",
        "RealtimeWorkflow has transport websocket only, connect, optional initial_messages, audio_stream, audio_message, receive_rules, optional finish_messages, completion, pause_behavior restart_session only, finalization_timeout_ms. Connect has url, optional query/headers, signer, optional subprotocol. RealtimeMessage is tagged json(value), text(value), binary(value). audio_stream supports pcm_s16le and pacing realtime. audio_message text/json must contain {{audio:chunk_base64}}; connect/query/headers/subprotocol/initial/finish may not use audio templates. Realtime receive_rules need an explicit complete action unless completion declares an event or JSONPath completion condition.\n",
        "A typed var preserves its native JSON type only when it is the complete value of a JSON leaf, exactly {{var:id}}. In URL, query, header, form, multipart text, raw bytes, realtime text/binary, or a mixed JSON string, vars render as text. multi_select, json_object, and json_array may be used only as that complete JSON leaf; reject them in every other template location before network I/O. json_object and json_array have no text serialization. Templates only allow {{var:id}}, {{secret:id}}, {{capture:id}}, {{audio:filename|mime|size|base64|data_uri|public_url|cloud_uri|chunk_base64}}, {{runtime:uuid|unix_seconds|unix_millis}}. Declarations, stage ordering, and delivery compatibility must validate. No other namespace, expression, code, file/environment access, or executable action exists."
    );
    format!("Advanced Audio API Workflow Schema v{CURRENT_SCHEMA_VERSION}\n\n{description}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parameter(id: &str) -> ParameterDefinition {
        ParameterDefinition {
            id: id.into(),
            label: id.into(),
            required: false,
            default: None,
            description: None,
            parameter_type: Some(ParameterType::Text),
            options: vec![],
            visible_when: None,
        }
    }

    fn secret(id: &str) -> SecretDefinition {
        SecretDefinition {
            id: id.into(),
            label: id.into(),
            required: false,
            description: None,
        }
    }

    fn stage() -> HttpStage {
        HttpStage {
            method: HttpMethod::Post,
            url: "https://asr.example.test/v1/transcribe".into(),
            query: BTreeMap::new(),
            headers: BTreeMap::new(),
            body: HttpBody::Json {
                value: serde_json::json!({"audio": "{{audio:base64}}", "model": "{{var:model}}"}),
            },
            accepted_statuses: vec![200],
            signer: SignerConfig::None,
            captures: vec![],
        }
    }

    fn workflow() -> AdvancedAudioWorkflow {
        AdvancedAudioWorkflow {
            schema_version: WorkflowSchemaVersion(CURRENT_SCHEMA_VERSION),
            name: "test".into(),
            parameters: vec![parameter("model")],
            secrets: vec![secret("key")],
            audio: AudioSpec {
                delivery: AudioDelivery::Base64,
                mime: Some("audio/wav".into()),
            },
            recognition: AdvancedRecognition::Request {
                request: Box::new(stage()),
                final_text: ResponseExtractor::JsonPath {
                    path: "$.text".into(),
                },
            },
        }
    }

    fn realtime_workflow() -> AdvancedAudioWorkflow {
        AdvancedAudioWorkflow {
            schema_version: WorkflowSchemaVersion(CURRENT_SCHEMA_VERSION),
            name: "realtime test".into(),
            parameters: vec![parameter("model")],
            secrets: vec![secret("key")],
            audio: AudioSpec {
                delivery: AudioDelivery::RealtimeChunks,
                mime: None,
            },
            recognition: AdvancedRecognition::RealtimeSession {
                realtime: Box::new(RealtimeWorkflow {
                    transport: RealtimeTransport::WebSocket,
                    connect: RealtimeConnect {
                        url: "wss://asr.example.test/realtime".into(),
                        query: BTreeMap::new(),
                        headers: BTreeMap::new(),
                        signer: SignerConfig::None,
                        subprotocol: None,
                    },
                    initial_messages: vec![],
                    audio_stream: RealtimeAudioStream {
                        codec: "pcm_s16le".into(),
                        sample_rate: 16_000,
                        channels: 1,
                        chunk_duration_ms: 80,
                        pacing: RealtimePacing::Realtime,
                    },
                    audio_message: RealtimeAudioMessage::Json {
                        value: serde_json::json!({"audio": "{{audio:chunk_base64}}"}),
                    },
                    receive_rules: vec![StreamRule {
                        event: None,
                        path: None,
                        action: StreamAction::Complete,
                        equals: None,
                    }],
                    finish_messages: vec![],
                    completion: RealtimeCompletion {
                        event: None,
                        path: None,
                        equals: None,
                    },
                    pause_behavior: PauseBehavior::RestartSession,
                    finalization_timeout_ms: 15_000,
                }),
            },
        }
    }

    #[test]
    fn accepts_a_bounded_request_workflow() {
        validate_workflow(&workflow()).unwrap();
    }

    #[test]
    fn rejects_unknown_schema_version_and_unsafe_templates() {
        let mut candidate = workflow();
        candidate.schema_version = WorkflowSchemaVersion(99);
        if let AdvancedRecognition::Request { request, .. } = &mut candidate.recognition {
            request
                .headers
                .insert("X-Test".into(), "{{env:HOME}}".into());
        }
        let error = validate_workflow(&candidate).unwrap_err().to_string();
        assert!(error.contains("Unsupported Workflow Schema Version 99"));
        assert!(error.contains("unknown template namespace 'env'"));
    }

    #[test]
    fn rejects_future_capture_references() {
        let mut candidate = workflow();
        if let AdvancedRecognition::Request { request, .. } = &mut candidate.recognition {
            request.url = "https://asr.example.test/{{capture:job}}".into();
            request.captures.push(Capture {
                id: "job".into(),
                from: ResponseExtractor::JsonPath {
                    path: "$.job".into(),
                },
                sensitive: false,
            });
        }
        assert!(
            validate_workflow(&candidate)
                .unwrap_err()
                .to_string()
                .contains("before it is created")
        );
    }

    #[test]
    fn allows_an_earlier_capture_as_an_entire_http_stage_url() {
        let mut submit = stage();
        submit.captures.push(Capture {
            id: "result_url".into(),
            from: ResponseExtractor::JsonPath {
                path: "$.result_url".into(),
            },
            sensitive: true,
        });
        let mut result = stage();
        result.method = HttpMethod::Get;
        result.url = "{{capture:result_url}}".into();
        result.body = HttpBody::None;
        let mut candidate = workflow();
        candidate.recognition = AdvancedRecognition::AsyncPoll {
            prepare: None,
            submit: Box::new(submit),
            poll: None,
            result_steps: vec![result],
            final_text: ResponseExtractor::PlainBody,
        };
        validate_workflow(&candidate).unwrap();

        let mut prefixed_capture = candidate.clone();
        let AdvancedRecognition::AsyncPoll { result_steps, .. } = &mut prefixed_capture.recognition
        else {
            unreachable!();
        };
        result_steps[0].url = "https://asr.example.test/{{capture:result_url}}".into();
        validate_workflow(&prefixed_capture).unwrap();

        for url in [
            "file:///tmp/transcript.json",
            "ftp://downloads.example.test/transcript.json",
            "/results/transcript.json",
            "{{var:model}}",
            "{{secret:key}}",
            "{{runtime:uuid}}",
            "{{capture:result_url}}/suffix",
        ] {
            let mut invalid = candidate.clone();
            let AdvancedRecognition::AsyncPoll { result_steps, .. } = &mut invalid.recognition
            else {
                unreachable!();
            };
            result_steps[0].url = url.into();
            assert!(
                validate_workflow(&invalid)
                    .unwrap_err()
                    .to_string()
                    .contains("must start with one of http, https://"),
                "expected {url:?} to be rejected"
            );
        }

        let mut future_capture = candidate;
        let AdvancedRecognition::AsyncPoll { submit, .. } = &mut future_capture.recognition else {
            unreachable!();
        };
        submit.captures.clear();
        assert!(
            validate_workflow(&future_capture)
                .unwrap_err()
                .to_string()
                .contains("references capture 'result_url' before it is created")
        );
    }

    #[test]
    fn config_is_inert_when_disabled_and_requires_values_when_enabled() {
        let config = AdvancedAudioConfig::default();
        validate_advanced_audio_config(&config).unwrap();
        let config = AdvancedAudioConfig {
            enabled: true,
            workflow: Some(AdvancedAudioWorkflow {
                parameters: vec![ParameterDefinition {
                    required: true,
                    ..parameter("model")
                }],
                secrets: vec![SecretDefinition {
                    required: true,
                    ..secret("key")
                }],
                ..workflow()
            }),
            ..Default::default()
        };
        let error = validate_advanced_audio_config(&config)
            .unwrap_err()
            .to_string();
        assert!(error.contains("required value is missing"));
        assert!(error.contains("required secret is missing"));
    }

    #[test]
    fn remote_delivery_requires_a_matching_publisher_and_keeps_cloud_uris_distinct() {
        let mut candidate = workflow();
        candidate.audio.delivery = AudioDelivery::PublicHttpsUrl;
        let AdvancedRecognition::Request { request, .. } = &mut candidate.recognition else {
            unreachable!();
        };
        request.body = HttpBody::Json {
            value: serde_json::json!({"audio": "{{audio:public_url}}"}),
        };
        let mut config = AdvancedAudioConfig {
            enabled: true,
            workflow: Some(candidate),
            ..Default::default()
        };
        assert!(
            validate_advanced_audio_config(&config)
                .unwrap_err()
                .to_string()
                .contains("remote hosting is required")
        );

        let workflow = config.workflow.as_mut().unwrap();
        workflow.audio.delivery = AudioDelivery::CloudUri;
        let AdvancedRecognition::Request { request, .. } = &mut workflow.recognition else {
            unreachable!();
        };
        request.body = HttpBody::Json {
            value: serde_json::json!({"audio": "{{audio:cloud_uri}}"}),
        };
        config.remote_audio = RemoteAudioConfig::Webdav(Default::default());
        assert!(
            validate_advanced_audio_config(&config)
                .unwrap_err()
                .to_string()
                .contains("WebDAV provides public HTTPS URLs, not cloud URIs")
        );
    }

    #[test]
    fn async_has_strict_stage_limit_and_realtime_is_websocket_only() {
        let mut candidate = workflow();
        candidate.recognition = AdvancedRecognition::AsyncPoll {
            prepare: None,
            submit: Box::new(stage()),
            poll: None,
            result_steps: vec![stage(), stage(), stage()],
            final_text: ResponseExtractor::PlainBody,
        };
        assert!(
            validate_workflow(&candidate)
                .unwrap_err()
                .to_string()
                .contains("at most 2 stages")
        );
    }

    #[test]
    fn request_stream_rejects_body_captures_before_sending_audio() {
        let mut candidate = workflow();
        let request = stage();
        candidate.recognition = AdvancedRecognition::RequestStream {
            request: Box::new(HttpStage {
                captures: vec![Capture {
                    id: "job_id".into(),
                    from: ResponseExtractor::JsonPath {
                        path: "$.id".into(),
                    },
                    sensitive: false,
                }],
                ..request
            }),
            stream: StreamResponse {
                format: StreamFormat::Ndjson,
                rules: vec![StreamRule {
                    event: None,
                    path: None,
                    action: StreamAction::Complete,
                    equals: None,
                }],
            },
        };
        let error = validate_workflow(&candidate).unwrap_err().to_string();
        assert!(error.contains("request_stream captures may only use header or status"));
    }

    #[test]
    fn requires_declared_audio_delivery_and_bounded_poll_requests() {
        let mut missing_audio = workflow();
        if let AdvancedRecognition::Request { request, .. } = &mut missing_audio.recognition {
            request.body = HttpBody::Json {
                value: serde_json::json!({"model": "{{var:model}}"}),
            };
        }
        assert!(
            validate_workflow(&missing_audio)
                .unwrap_err()
                .to_string()
                .contains("base64 delivery must be used")
        );

        let mut invalid_poll = workflow();
        invalid_poll.recognition = AdvancedRecognition::AsyncPoll {
            prepare: None,
            submit: Box::new(stage()),
            poll: Some(Box::new(PollStage {
                request: HttpStage {
                    method: HttpMethod::Put,
                    body: HttpBody::None,
                    ..stage()
                },
                interval_ms: 100,
                timeout_ms: 1_000,
                pending: vec![PollCondition {
                    from: ResponseExtractor::Status,
                    operator: PollOperator::Eq,
                    value: Some("202".into()),
                    values: vec![],
                }],
                success: vec![PollCondition {
                    from: ResponseExtractor::Status,
                    operator: PollOperator::Eq,
                    value: Some("200".into()),
                    values: vec![],
                }],
                failure: vec![PollCondition {
                    from: ResponseExtractor::Status,
                    operator: PollOperator::Eq,
                    value: Some("400".into()),
                    values: vec![],
                }],
            })),
            result_steps: vec![],
            final_text: ResponseExtractor::PlainBody,
        };
        assert!(
            validate_workflow(&invalid_poll)
                .unwrap_err()
                .to_string()
                .contains("poll requests support only GET or POST")
        );
    }

    #[test]
    fn provider_upload_requires_a_prepare_capture_referenced_by_submit() {
        let mut candidate = workflow();
        candidate.audio.delivery = AudioDelivery::ProviderUpload;
        candidate.recognition = AdvancedRecognition::AsyncPoll {
            prepare: None,
            submit: Box::new(stage()),
            poll: None,
            result_steps: vec![],
            final_text: ResponseExtractor::PlainBody,
        };
        assert!(
            validate_workflow(&candidate)
                .unwrap_err()
                .to_string()
                .contains("provider_upload requires an async_poll prepare upload stage")
        );
    }

    #[test]
    fn rejects_an_oversized_serialized_workflow() {
        let mut candidate = workflow();
        if let AdvancedRecognition::Request { request, .. } = &mut candidate.recognition {
            request.body = HttpBody::RawBytes {
                value: "x".repeat(MAX_WORKFLOW_JSON_BYTES),
            };
        }
        let error = validate_workflow(&candidate).unwrap_err().to_string();
        assert!(error.contains("serialized workflow exceeds"));
    }

    #[test]
    fn rejects_template_parameter_defaults_that_would_not_be_rendered() {
        let mut candidate = workflow();
        candidate.parameters[0].default = Some("{{runtime:uuid}}".into());
        let error = validate_workflow(&candidate).unwrap_err().to_string();
        assert!(error.contains("parameter defaults may not contain template placeholders"));
    }

    #[test]
    fn retains_version_one_text_only_workflows_without_automatic_upgrade() {
        let mut legacy = workflow();
        legacy.schema_version = WorkflowSchemaVersion(LEGACY_SCHEMA_VERSION);
        legacy.parameters[0].parameter_type = None;
        validate_workflow(&legacy).unwrap();

        let config = AdvancedAudioConfig {
            enabled: true,
            workflow: Some(legacy.clone()),
            values: BTreeMap::from([("model".into(), "16000".into())]),
            ..Default::default()
        };
        let persisted = serde_json::to_value(&config).unwrap();
        assert_eq!(persisted["values"]["model"], serde_json::json!("16000"));
        let reloaded: AdvancedAudioConfig = serde_json::from_value(persisted).unwrap();
        validate_advanced_audio_config(&reloaded).unwrap();
        assert_eq!(
            reloaded.workflow.unwrap().schema_version.0,
            LEGACY_SCHEMA_VERSION
        );

        legacy.parameters[0].parameter_type = Some(ParameterType::Text);
        let error = validate_workflow(&legacy).unwrap_err().to_string();
        assert!(error.contains("is only supported by workflow schema version 2"));
    }

    #[test]
    fn validates_version_two_typed_defaults_and_configured_values() {
        let options = vec![
            ParameterOption {
                value: "fast".into(),
                label: "Fast".into(),
            },
            ParameterOption {
                value: "accurate".into(),
                label: "Accurate".into(),
            },
        ];
        let mut integer = parameter("sample_rate");
        integer.parameter_type = Some(ParameterType::Integer);
        integer.default = Some("16000".into());
        let mut number = parameter("temperature");
        number.parameter_type = Some(ParameterType::Number);
        number.default = Some("0.25".into());
        let mut boolean = parameter("itn");
        boolean.parameter_type = Some(ParameterType::Boolean);
        boolean.default = Some("true".into());
        let mut select = parameter("mode");
        select.parameter_type = Some(ParameterType::Select);
        select.options = options.clone();
        select.default = Some("fast".into());
        let mut multi = parameter("languages");
        multi.parameter_type = Some(ParameterType::MultiSelect);
        multi.options = options;
        multi.default = Some(r#"["fast","accurate"]"#.into());
        let mut vocabulary = parameter("vocabulary");
        vocabulary.parameter_type = Some(ParameterType::JsonObject);
        vocabulary.default = Some(r#"{"dictate":"Dictate"}"#.into());
        let mut language_hints = parameter("language_hints");
        language_hints.parameter_type = Some(ParameterType::JsonArray);
        language_hints.default = Some(r#"["zh","en"]"#.into());

        let mut candidate = workflow();
        candidate.parameters = vec![
            parameter("model"),
            integer,
            number,
            boolean,
            select,
            multi,
            vocabulary,
            language_hints,
        ];
        validate_workflow(&candidate).unwrap();

        let config = AdvancedAudioConfig {
            enabled: true,
            workflow: Some(candidate),
            values: BTreeMap::from([
                ("sample_rate".into(), "8000".into()),
                ("temperature".into(), "0.5".into()),
                ("itn".into(), "false".into()),
                ("mode".into(), "accurate".into()),
                ("languages".into(), r#"["accurate"]"#.into()),
                ("vocabulary".into(), r#"{"hotword":"Dictate"}"#.into()),
                (
                    "language_hints".into(),
                    r#"["zh",{"language":"en"}]"#.into(),
                ),
            ]),
            ..Default::default()
        };
        validate_advanced_audio_config(&config).unwrap();

        let mut invalid = config.clone();
        invalid
            .values
            .insert("sample_rate".into(), "16000.5".into());
        let error = validate_advanced_audio_config(&invalid)
            .unwrap_err()
            .to_string();
        assert!(error.contains("ADVANCED_AUDIO_API.values.sample_rate"));
        assert!(error.contains("must be a JSON integer"));

        let mut invalid_object = config.clone();
        invalid_object.values.insert(
            "vocabulary".into(),
            r#"["value-must-not-appear-in-error"]"#.into(),
        );
        let object_error = validate_advanced_audio_config(&invalid_object)
            .unwrap_err()
            .to_string();
        assert!(object_error.contains("ADVANCED_AUDIO_API.values.vocabulary"));
        assert!(object_error.contains("must be a JSON object"));
        assert!(!object_error.contains("value-must-not-appear-in-error"));

        let mut invalid_array = config;
        invalid_array.values.insert(
            "language_hints".into(),
            r#"{"value":"value-must-not-appear-in-error"}"#.into(),
        );
        let array_error = validate_advanced_audio_config(&invalid_array)
            .unwrap_err()
            .to_string();
        assert!(array_error.contains("ADVANCED_AUDIO_API.values.language_hints"));
        assert!(array_error.contains("must be a JSON array"));
        assert!(!array_error.contains("value-must-not-appear-in-error"));
    }

    #[test]
    fn json_container_parameters_require_matching_values_but_allow_empty_containers() {
        let mut vocabulary = parameter("vocabulary");
        vocabulary.required = true;
        vocabulary.parameter_type = Some(ParameterType::JsonObject);
        let mut language_hints = parameter("language_hints");
        language_hints.required = true;
        language_hints.parameter_type = Some(ParameterType::JsonArray);

        let mut candidate = workflow();
        candidate.parameters = vec![parameter("model"), vocabulary, language_hints];
        validate_workflow(&candidate).unwrap();

        let config = AdvancedAudioConfig {
            enabled: true,
            workflow: Some(candidate),
            values: BTreeMap::from([
                ("vocabulary".into(), "{}".into()),
                ("language_hints".into(), "[]".into()),
            ]),
            ..Default::default()
        };
        validate_advanced_audio_config(&config).unwrap();

        let mut missing = config;
        missing.values.remove("vocabulary");
        let error = validate_advanced_audio_config(&missing)
            .unwrap_err()
            .to_string();
        assert!(error.contains("ADVANCED_AUDIO_API.values.vocabulary: required value is missing"));
    }

    #[test]
    fn rejects_options_for_json_container_parameters() {
        let option = ParameterOption {
            value: "unexpected".into(),
            label: "Unexpected".into(),
        };
        let mut vocabulary = parameter("vocabulary");
        vocabulary.parameter_type = Some(ParameterType::JsonObject);
        vocabulary.options = vec![option.clone()];
        let mut language_hints = parameter("language_hints");
        language_hints.parameter_type = Some(ParameterType::JsonArray);
        language_hints.options = vec![option];

        let mut candidate = workflow();
        candidate.parameters = vec![parameter("model"), vocabulary, language_hints];
        let error = validate_workflow(&candidate).unwrap_err().to_string();
        assert!(error.contains(
            "parameters[1].options: is allowed only for select or multi_select parameters"
        ));
        assert!(error.contains(
            "parameters[2].options: is allowed only for select or multi_select parameters"
        ));
    }

    #[test]
    fn rejects_invalid_version_two_option_and_visibility_definitions() {
        let mut source = parameter("mode");
        source.parameter_type = Some(ParameterType::Select);
        source.default = Some("fast".into());
        source.options = vec![
            ParameterOption {
                value: "fast".into(),
                label: "Fast".into(),
            },
            ParameterOption {
                value: "fast".into(),
                label: "Duplicate".into(),
            },
        ];
        let mut dependent = parameter("hint");
        dependent.visible_when = Some(VisibilityCondition {
            parameter: "mode".into(),
            equals: Some("fast".into()),
            one_of: vec!["accurate".into()],
        });
        let mut candidate = workflow();
        candidate.parameters = vec![parameter("model"), source, dependent];

        let error = validate_workflow(&candidate).unwrap_err().to_string();
        assert!(error.contains("duplicate option value 'fast'"));
        assert!(error.contains("exactly one of equals or a nonempty one_of array"));
    }

    #[test]
    fn accepts_bounded_visibility_conditions_without_relaxing_required_values() {
        let mut mode = parameter("mode");
        mode.parameter_type = Some(ParameterType::Select);
        mode.default = Some("fast".into());
        mode.options = vec![
            ParameterOption {
                value: "fast".into(),
                label: "Fast".into(),
            },
            ParameterOption {
                value: "accurate".into(),
                label: "Accurate".into(),
            },
        ];
        let mut hint = parameter("hint");
        hint.required = true;
        hint.visible_when = Some(VisibilityCondition {
            parameter: "mode".into(),
            equals: None,
            one_of: vec!["fast".into(), "accurate".into()],
        });
        let mut candidate = workflow();
        candidate.parameters = vec![parameter("model"), mode, hint];
        validate_workflow(&candidate).unwrap();

        let config = AdvancedAudioConfig {
            enabled: true,
            workflow: Some(candidate),
            ..Default::default()
        };
        let error = validate_advanced_audio_config(&config)
            .unwrap_err()
            .to_string();
        assert!(error.contains("ADVANCED_AUDIO_API.values.hint: required value is missing"));
    }

    #[test]
    fn rejects_visibility_condition_source_without_a_default() {
        let mut source = parameter("enable_extra");
        source.parameter_type = Some(ParameterType::Boolean);
        let mut dependent = parameter("extra_value");
        dependent.visible_when = Some(VisibilityCondition {
            parameter: "enable_extra".into(),
            equals: Some("true".into()),
            one_of: vec![],
        });

        let mut candidate = workflow();
        candidate.parameters = vec![parameter("model"), source, dependent];
        let error = validate_workflow(&candidate).unwrap_err().to_string();
        assert!(error.contains("must reference a parameter with a default value"));
    }

    #[test]
    fn rejects_non_boolean_or_select_visibility_sources() {
        let invalid_sources = [
            (ParameterType::Text, "enabled", vec![]),
            (ParameterType::Number, "1", vec![]),
            (ParameterType::JsonObject, r#"{"enabled":true}"#, vec![]),
            (ParameterType::JsonArray, r#"["enabled"]"#, vec![]),
            (
                ParameterType::MultiSelect,
                r#"[\"zh\"]"#,
                vec![ParameterOption {
                    value: "zh".into(),
                    label: "Chinese".into(),
                }],
            ),
        ];

        for (parameter_type, default, options) in invalid_sources {
            let mut source = parameter("source");
            source.parameter_type = Some(parameter_type);
            source.default = Some(default.into());
            source.options = options;
            let mut dependent = parameter("dependent");
            dependent.visible_when = Some(VisibilityCondition {
                parameter: "source".into(),
                equals: Some(default.into()),
                one_of: vec![],
            });

            let mut candidate = workflow();
            candidate.parameters = vec![parameter("model"), source, dependent];
            let error = validate_workflow(&candidate).unwrap_err().to_string();
            assert!(
                error.contains("must reference a boolean or select parameter"),
                "{parameter_type:?} must not be a visibility source: {error}"
            );
        }
    }

    #[test]
    fn rejects_conditional_or_later_visibility_sources() {
        let mut gate = parameter("gate");
        gate.parameter_type = Some(ParameterType::Boolean);
        gate.default = Some("true".into());
        let mut conditional_source = parameter("conditional_source");
        conditional_source.parameter_type = Some(ParameterType::Boolean);
        conditional_source.default = Some("true".into());
        conditional_source.visible_when = Some(VisibilityCondition {
            parameter: "gate".into(),
            equals: Some("true".into()),
            one_of: vec![],
        });
        let mut dependent = parameter("dependent");
        dependent.visible_when = Some(VisibilityCondition {
            parameter: "conditional_source".into(),
            equals: Some("true".into()),
            one_of: vec![],
        });

        let mut conditional_candidate = workflow();
        conditional_candidate.parameters =
            vec![parameter("model"), gate, conditional_source, dependent];
        let error = validate_workflow(&conditional_candidate)
            .unwrap_err()
            .to_string();
        assert!(error.contains("must reference an unconditional parameter"));

        let mut later = parameter("later");
        later.parameter_type = Some(ParameterType::Boolean);
        later.default = Some("true".into());
        let mut earlier_dependent = parameter("earlier_dependent");
        earlier_dependent.visible_when = Some(VisibilityCondition {
            parameter: "later".into(),
            equals: Some("true".into()),
            one_of: vec![],
        });

        let mut later_candidate = workflow();
        later_candidate.parameters = vec![parameter("model"), earlier_dependent, later];
        let error = validate_workflow(&later_candidate).unwrap_err().to_string();
        assert!(
            error.contains("must reference an earlier unconditional boolean or select parameter")
        );
    }

    #[test]
    fn rejects_visibility_comparisons_outside_the_source_domain() {
        let mut enabled = parameter("enabled");
        enabled.parameter_type = Some(ParameterType::Boolean);
        enabled.default = Some("true".into());
        let mut boolean_dependent = parameter("boolean_dependent");
        boolean_dependent.visible_when = Some(VisibilityCondition {
            parameter: "enabled".into(),
            equals: Some("enabled".into()),
            one_of: vec![],
        });

        let mut boolean_candidate = workflow();
        boolean_candidate.parameters = vec![parameter("model"), enabled, boolean_dependent];
        let error = validate_workflow(&boolean_candidate)
            .unwrap_err()
            .to_string();
        assert!(error.contains("must be a declared value of the referenced parameter"));

        let mut mode = parameter("mode");
        mode.parameter_type = Some(ParameterType::Select);
        mode.default = Some("fast".into());
        mode.options = vec![ParameterOption {
            value: "fast".into(),
            label: "Fast".into(),
        }];
        let mut select_dependent = parameter("select_dependent");
        select_dependent.visible_when = Some(VisibilityCondition {
            parameter: "mode".into(),
            equals: Some("turbo".into()),
            one_of: vec![],
        });

        let mut select_candidate = workflow();
        select_candidate.parameters = vec![parameter("model"), mode, select_dependent];
        let error = validate_workflow(&select_candidate)
            .unwrap_err()
            .to_string();
        assert!(error.contains("must be a declared value of the referenced parameter"));
    }

    #[test]
    fn remote_validation_helper_does_not_need_placeholder_parameter_values() {
        let mut candidate = workflow();
        candidate.parameters[0].required = true;
        validate_remote_audio_config(&candidate, &RemoteAudioConfig::None).unwrap();

        let config = AdvancedAudioConfig {
            enabled: true,
            workflow: Some(candidate),
            ..Default::default()
        };
        assert!(validate_advanced_audio_config(&config).is_err());
    }

    #[test]
    fn realtime_allows_audio_only_for_per_chunk_messages() {
        let mut candidate = realtime_workflow();
        let AdvancedRecognition::RealtimeSession { realtime } = &mut candidate.recognition else {
            unreachable!();
        };
        realtime
            .connect
            .query
            .insert("filename".into(), "{{audio:filename}}".into());
        realtime.initial_messages.push(RealtimeMessage::Text {
            value: "{{audio:chunk_base64}}".into(),
        });
        realtime.audio_message = RealtimeAudioMessage::Text {
            value: "{{audio:filename}}:{{audio:chunk_base64}}".into(),
        };
        let error = validate_workflow(&candidate).unwrap_err().to_string();
        assert!(error.contains("recognition.realtime.connect.query.filename"));
        assert!(error.contains("recognition.realtime.initial_messages[0]"));
        assert!(error.contains("recognition.realtime.audio_message.value"));
    }

    #[test]
    fn rejects_signers_for_streaming_upload_bodies_before_execution() {
        let mut candidate = workflow();
        candidate.audio.delivery = AudioDelivery::RawAudio;
        candidate
            .secrets
            .extend([secret("access"), secret("signing")]);
        let AdvancedRecognition::Request { request, .. } = &mut candidate.recognition else {
            unreachable!();
        };
        request.body = HttpBody::RawAudio;
        request.signer = SignerConfig::AwsSigv4 {
            region: "us-east-1".into(),
            service: "execute-api".into(),
            access_key_secret: "access".into(),
            secret_key_secret: "signing".into(),
            session_token_secret: None,
        };
        let error = validate_workflow(&candidate).unwrap_err().to_string();
        assert!(error.contains("cannot be used with multipart or raw_audio"));
    }

    #[test]
    fn complete_rule_can_match_a_terminal_value_but_equals_needs_a_path() {
        let mut candidate = workflow();
        let request = match &candidate.recognition {
            AdvancedRecognition::Request { request, .. } => request.clone(),
            _ => unreachable!(),
        };
        candidate.recognition = AdvancedRecognition::RequestStream {
            request,
            stream: StreamResponse {
                format: StreamFormat::Ndjson,
                rules: vec![StreamRule {
                    event: None,
                    path: Some("$.done".into()),
                    action: StreamAction::Complete,
                    equals: Some("true".into()),
                }],
            },
        };
        validate_workflow(&candidate).unwrap();

        if let AdvancedRecognition::RequestStream { stream, .. } = &mut candidate.recognition {
            stream.rules[0].path = None;
        }
        let error = validate_workflow(&candidate).unwrap_err().to_string();
        assert!(error.contains("requires path so a value can be compared"));
    }

    #[test]
    fn realtime_completion_condition_can_replace_a_duplicate_complete_rule() {
        let mut candidate = realtime_workflow();
        {
            let AdvancedRecognition::RealtimeSession { realtime } = &mut candidate.recognition
            else {
                unreachable!();
            };
            realtime.receive_rules = vec![StreamRule {
                event: Some("final".into()),
                path: Some("$.text".into()),
                action: StreamAction::SetFinalText,
                equals: None,
            }];
            realtime.completion = RealtimeCompletion {
                event: Some("done".into()),
                path: None,
                equals: None,
            };
        }

        validate_workflow(&candidate).unwrap();

        let AdvancedRecognition::RealtimeSession { realtime } = &mut candidate.recognition else {
            unreachable!();
        };
        realtime.completion = RealtimeCompletion {
            event: None,
            path: None,
            equals: None,
        };
        assert!(
            validate_workflow(&candidate)
                .unwrap_err()
                .to_string()
                .contains("must include a complete action")
        );
    }

    #[test]
    fn schema_description_explains_dynamic_captured_http_urls() {
        let description = workflow_schema_description();
        assert!(description.contains("including async_poll result_steps"));
        assert!(description.contains("must start with http:// or https:// before any template"));
        assert!(description.contains("may be exactly {{capture:id}}"));
        assert!(
            description.contains(
                "rendered value is checked for an absolute HTTP(S) URL before the request"
            )
        );
        assert!(description.contains("automatically treated as sensitive for redaction"));
        assert!(description.contains("Relative paths and other template-only URLs are invalid"));
    }

    #[test]
    fn schema_description_explains_typed_parameter_storage_and_rendering() {
        let description = workflow_schema_description();
        assert!(description.contains("All parameter defaults and saved values are strings"));
        assert!(description.contains("json_object|json_array"));
        assert!(description.contains("A multi_select default or saved value is a string"));
        assert!(description.contains("never emit an actual JSON array for its default"));
        assert!(
            description.contains("json_object and json_array defaults or saved values are strings")
        );
        assert!(description.contains("complete value of a JSON leaf, exactly {{var:id}}"));
        assert!(description.contains(
            "multi_select, json_object, and json_array may be used only as that complete JSON leaf"
        ));
        assert!(description.contains("json_object and json_array have no text serialization"));
        assert!(description.contains("before network I/O"));
    }
}
