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
    AudioDeliveryType, HttpBody, HttpMethod, HttpStage, ParameterDefinition, RemoteAudioConfig,
    SecretDefinition, WorkflowSchemaVersion,
};
pub use validation::{
    ValidationError, ValidationErrors, validate_advanced_audio_config, validate_workflow,
    workflow_schema_description,
};

/// The only workflow version this build understands.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;
