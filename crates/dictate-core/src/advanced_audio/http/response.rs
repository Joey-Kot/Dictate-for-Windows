use futures_util::StreamExt;
use reqwest::header::HeaderMap;
use reqwest::{Response, StatusCode};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use super::MAX_RESPONSE_BYTES;

/// A bounded, materialized HTTP response.  The original `Response` is used
/// directly only for response-stream workflows.
#[derive(Debug, Clone)]
pub struct HttpResponseData {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl HttpResponseData {
    pub fn is_accepted(&self, accepted: &[u16]) -> bool {
        accepted
            .iter()
            .any(|status| *status == self.status.as_u16())
    }
}

pub async fn read_response_limited(
    response: Response,
    cancellation: &CancellationToken,
) -> Result<HttpResponseData, ResponseReadError> {
    let status = response.status();
    let headers = response.headers().clone();
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    loop {
        let next = tokio::select! {
            _ = cancellation.cancelled() => return Err(ResponseReadError::Canceled),
            item = stream.next() => item,
        };
        let Some(next) = next else {
            break;
        };
        let bytes = next.map_err(ResponseReadError::Body)?;
        if body.len().saturating_add(bytes.len()) > MAX_RESPONSE_BYTES {
            return Err(ResponseReadError::TooLarge {
                maximum: MAX_RESPONSE_BYTES,
            });
        }
        body.extend_from_slice(&bytes);
    }
    Ok(HttpResponseData {
        status,
        headers,
        body,
    })
}

#[derive(Debug, Error)]
pub enum ResponseReadError {
    #[error("request canceled")]
    Canceled,
    // A streaming transport error can retain the rendered endpoint URL.
    // Preserve it only as an error source, never in Display output.
    #[error("failed to read HTTP response")]
    Body(#[source] reqwest::Error),
    #[error("HTTP response exceeds the {maximum}-byte limit")]
    TooLarge { maximum: usize },
}
