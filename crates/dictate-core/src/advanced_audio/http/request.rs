use std::collections::BTreeMap;

use chrono::Utc;
use reqwest::{Client, Method, Response, Url};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use super::body::{BodyError, BuiltBody, StageContext, build_body};
use crate::advanced_audio::auth::{BuiltinRequestSigner, SignerError, SigningRequest};
use crate::advanced_audio::schema::{HttpMethod, HttpStage, SignerConfig};
use crate::advanced_audio::template::TemplateError;

/// Executes validated HTTP stages using the application's shared network
/// configuration.  Signers are kept in Core and deliberately cannot be
/// supplied as workflow code.
#[derive(Clone)]
pub struct HttpEngine {
    client: Client,
}

impl HttpEngine {
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    pub async fn send(
        &self,
        stage: &HttpStage,
        context: &StageContext<'_>,
        cancellation: &CancellationToken,
    ) -> Result<Response, HttpEngineError> {
        if cancellation.is_cancelled() {
            return Err(HttpEngineError::Canceled);
        }
        let url = parse_rendered_http_url(&context.render(&stage.url)?)?;
        let mut request = self.client.request(method(stage.method), url);
        let query = render_pairs(&stage.query, context)?;
        if !query.is_empty() {
            request = request.query(&query);
        }
        for (name, value) in render_pairs(&stage.headers, context)? {
            request = request.header(name, value);
        }
        request = request.header(
            reqwest::header::USER_AGENT,
            "dictate-client/advanced-audio-v1",
        );
        request = match build_body(&stage.body, context).await? {
            BuiltBody::Empty => request,
            BuiltBody::Json(value) => request.json(&value),
            BuiltBody::Form(values) => request.form(&values),
            BuiltBody::Multipart(form) => request.multipart(form),
            BuiltBody::RawAudio(body) => request
                .header(reqwest::header::CONTENT_TYPE, context.audio.mime())
                .body(body),
            BuiltBody::RawBytes(bytes) => request.body(bytes),
        };
        let mut request = request.build().map_err(HttpEngineError::Request)?;
        sign_request(&mut request, &stage.signer, context.secrets)?;
        tokio::select! {
            _ = cancellation.cancelled() => Err(HttpEngineError::Canceled),
            response = self.client.execute(request) => response.map_err(HttpEngineError::Request),
        }
    }
}

fn method(method: HttpMethod) -> Method {
    match method {
        HttpMethod::Get => Method::GET,
        HttpMethod::Post => Method::POST,
        HttpMethod::Put => Method::PUT,
        HttpMethod::Patch => Method::PATCH,
        HttpMethod::Delete => Method::DELETE,
    }
}

fn parse_rendered_http_url(value: &str) -> Result<Url, HttpEngineError> {
    let url = Url::parse(value).map_err(|_| HttpEngineError::InvalidUrl)?;
    let supported_scheme = ["http", "https"]
        .iter()
        .any(|scheme| url.scheme().eq_ignore_ascii_case(scheme));
    if !supported_scheme || url.host_str().is_none() {
        return Err(HttpEngineError::InvalidUrl);
    }
    Ok(url)
}

fn render_pairs(
    fields: &BTreeMap<String, String>,
    context: &StageContext<'_>,
) -> Result<Vec<(String, String)>, HttpEngineError> {
    let mut rendered = Vec::with_capacity(fields.len());
    for (key, value) in fields {
        rendered.push((key.clone(), context.render(value)?));
    }
    Ok(rendered)
}

fn sign_request(
    request: &mut reqwest::Request,
    signer_config: &SignerConfig,
    secrets: &BTreeMap<String, String>,
) -> Result<(), HttpEngineError> {
    if matches!(signer_config, SignerConfig::None) {
        return Ok(());
    }
    // RequestBuilder materializes JSON/form/raw-bytes bodies. Multipart and
    // raw audio deliberately remain streaming for normal workflows; signing
    // them would require hashing bytes that are not available to the signer.
    // Reject rather than signing different bytes from the actual upload.
    let body = match request.body() {
        None => Vec::new(),
        Some(body) => body
            .as_bytes()
            .map(ToOwned::to_owned)
            .ok_or(HttpEngineError::SignerRequiresMaterializedBody)?,
    };
    let signer = BuiltinRequestSigner::from_config(signer_config, secrets)?;
    let method = request.method().clone();
    let url = request.url().clone();
    let mut signing = SigningRequest::new(&method, &url, request.headers_mut(), &body, Utc::now());
    signer.sign(&mut signing)?;
    Ok(())
}

#[derive(Debug, Error)]
pub enum HttpEngineError {
    #[error("request canceled")]
    Canceled,
    // reqwest includes the fully rendered request URL in its Display output.
    // That URL may contain a workflow secret, a capture, or a presigned
    // credential, so keep the source available for internal inspection but
    // never place it in a user-facing/loggable error string.
    #[error("HTTP request failed")]
    Request(#[source] reqwest::Error),
    #[error("rendered HTTP stage URL must be an absolute http or https URL")]
    InvalidUrl,
    #[error("{0}")]
    Body(#[from] BodyError),
    #[error("{0}")]
    Template(#[from] TemplateError),
    #[error(
        "dynamic signing requires a materialized request body; multipart and raw audio remain streaming"
    )]
    SignerRequiresMaterializedBody,
    #[error("request signing failed: {0}")]
    Signing(#[from] SignerError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendered_http_urls_require_an_absolute_http_scheme_and_host() {
        for value in [
            "http://127.0.0.1:8080/result.json",
            "https://downloads.example.test/result.json?signature=opaque",
        ] {
            let url = parse_rendered_http_url(value).unwrap();
            assert!(matches!(url.scheme(), "http" | "https"));
            assert!(url.host_str().is_some());
        }

        for value in [
            "file:///tmp/result.json",
            "ftp://downloads.example.test/result.json",
            "/results/final.json",
            "https://",
        ] {
            assert!(matches!(
                parse_rendered_http_url(value),
                Err(HttpEngineError::InvalidUrl)
            ));
        }
    }
}
