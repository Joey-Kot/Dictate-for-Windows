//! Bounded HTTP primitives shared by request, stream, and async workflows.

mod body;
mod request;
mod response;

pub use body::{BodyError, PreparedAudio, StageContext, StageContextInputs};
pub(crate) use body::{
    TypedTemplateRenderError, render_json_with_context, render_text_with_context,
};
pub use request::{HttpEngine, HttpEngineError};
pub use response::{HttpResponseData, ResponseReadError, read_response_limited};

/// Maximum response body accepted from an Advanced workflow.  A workflow is a
/// transcription protocol, not an unrestricted downloader.
pub const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

/// Base64/Data-URI is intentionally bounded because it must be materialized
/// in memory before it can be put in JSON.
pub const MAX_BASE64_AUDIO_BYTES: u64 = 16 * 1024 * 1024;
