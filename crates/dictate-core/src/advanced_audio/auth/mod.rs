//! Built-in authentication helpers for Advanced Audio API requests.
//!
//! Static authentication stays in workflow templates, for example
//! `Authorization: Bearer {{secret:api_key}}`.  This module is only for the
//! bounded signing algorithms which cannot safely be expressed as templates.

mod aws_sigv4;
mod signer;
mod tencent_tc3;

pub use aws_sigv4::AwsSigV4Signer;
pub use signer::{BuiltinRequestSigner, RequestSigner, SignerError, SigningRequest};
pub use tencent_tc3::TencentTc3Signer;
