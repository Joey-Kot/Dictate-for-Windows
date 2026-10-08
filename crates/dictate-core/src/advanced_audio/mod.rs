//! Declarative, validated Advanced Audio API workflows.
//!
//! This module intentionally models an ASR protocol rather than a list of
//! providers.  Frontends create and edit workflows, while this crate owns
//! validation and execution.  Keeping the schema here prevents GUI and CLI
//! implementations from drifting apart.

pub mod accumulator;
pub mod auth;
pub mod client;
pub mod extractor;
pub mod http;
pub mod realtime;
pub mod remote_audio;
pub mod request_stream;
pub mod schema;
pub mod template;
pub mod validation;

pub use client::{AdvancedAudioClient, AdvancedAudioError};
pub use realtime::{LiveChunkSource, RealtimeSessionError, RecordedReplaySource};
pub use remote_audio::{RemoteAudioError, RemoteAudioPublisher, RemoteAudioReference};
pub use schema::{
    AdvancedAudioConfig, AdvancedAudioWorkflow, AdvancedRecognition, AudioDelivery,
    AudioDeliveryType, HttpBody, HttpMethod, HttpStage, ParameterDefinition, ParameterOption,
    ParameterType, ParameterValueError, RemoteAudioConfig, SecretDefinition, VisibilityCondition,
    WorkflowSchemaVersion,
};
pub use validation::{
    ValidationError, ValidationErrors, validate_advanced_audio_config,
    validate_remote_audio_config, validate_workflow, workflow_schema_description,
};

/// The text-only workflow schema retained for existing saved configurations.
pub const LEGACY_SCHEMA_VERSION: u32 = 1;

/// The newest workflow schema emitted by the GUI compiler.
pub const CURRENT_SCHEMA_VERSION: u32 = 2;

/// Returns whether a workflow version has a stable implementation in this
/// build.  Unknown versions are never guessed or executed.
pub const fn is_supported_schema_version(version: u32) -> bool {
    matches!(version, LEGACY_SCHEMA_VERSION | CURRENT_SCHEMA_VERSION)
}
