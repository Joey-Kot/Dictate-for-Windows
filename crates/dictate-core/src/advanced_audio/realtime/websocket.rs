use std::collections::BTreeMap;

use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use thiserror::Error;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::{Message, WebSocketConfig};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async_with_config};
use tokio_util::sync::CancellationToken;

use crate::advanced_audio::auth::{BuiltinRequestSigner, SigningRequest};
use crate::advanced_audio::schema::RealtimeConnect;
use crate::advanced_audio::template::{
    AudioTemplateValues, RuntimeTemplateValues, Template, TemplateContext, TemplateError,
};

const MAX_WEBSOCKET_MESSAGE_BYTES: usize = 32 * 1024 * 1024;

pub(super) type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Owns one bounded WebSocket connection. The session module maps workflow
/// messages and transcript rules; this transport only handles connect/send/
/// receive/close/cancellation.
pub(super) struct WebSocketTransport {
    socket: Socket,
}

impl WebSocketTransport {
    pub(super) async fn connect(
        connect: &RealtimeConnect,
        values: &BTreeMap<String, String>,
        secrets: &BTreeMap<String, String>,
        runtime: &RuntimeTemplateValues,
        cancellation: &CancellationToken,
    ) -> Result<Self, WebSocketError> {
        if cancellation.is_cancelled() {
            return Err(WebSocketError::Canceled);
        }
        let context = template_context(values, secrets, runtime, None);
        let raw_url = render(&connect.url, &context)?;
        let mut url = reqwest::Url::parse(&raw_url).map_err(|_| WebSocketError::InvalidUrl)?;
        {
            let mut pairs = url.query_pairs_mut();
            for (name, value) in &connect.query {
                pairs.append_pair(name, &render(value, &context)?);
            }
        }

        let mut signing_headers = reqwest::header::HeaderMap::new();
        for (name, value) in &connect.headers {
            let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| WebSocketError::InvalidHeaderName)?;
            let value = reqwest::header::HeaderValue::from_str(&render(value, &context)?)
                .map_err(|_| WebSocketError::InvalidHeaderValue)?;
            signing_headers.insert(name, value);
        }
        signing_headers.insert(
            reqwest::header::USER_AGENT,
            reqwest::header::HeaderValue::from_static("dictate-client/advanced-audio-v1"),
        );
        if let Some(subprotocol) = &connect.subprotocol {
            let value = reqwest::header::HeaderValue::from_str(&render(subprotocol, &context)?)
                .map_err(|_| WebSocketError::InvalidHeaderValue)?;
            signing_headers.insert(
                reqwest::header::HeaderName::from_static("sec-websocket-protocol"),
                value,
            );
        }
        let signer = BuiltinRequestSigner::from_config(&connect.signer, secrets)
            .map_err(|_| WebSocketError::Signing)?;
        let method = reqwest::Method::GET;
        let mut signing_request =
            SigningRequest::new(&method, &url, &mut signing_headers, &[], Utc::now());
        signer
            .sign(&mut signing_request)
            .map_err(|_| WebSocketError::Signing)?;

        let mut request = url
            .as_str()
            .into_client_request()
            .map_err(|_| WebSocketError::InvalidUrl)?;
        for (name, value) in &signing_headers {
            let name = tokio_tungstenite::tungstenite::http::HeaderName::from_bytes(
                name.as_str().as_bytes(),
            )
            .map_err(|_| WebSocketError::InvalidHeaderName)?;
            let value =
                tokio_tungstenite::tungstenite::http::HeaderValue::from_bytes(value.as_bytes())
                    .map_err(|_| WebSocketError::InvalidHeaderValue)?;
            request.headers_mut().insert(name, value);
        }
        let config = WebSocketConfig {
            max_message_size: Some(MAX_WEBSOCKET_MESSAGE_BYTES),
            max_frame_size: Some(MAX_WEBSOCKET_MESSAGE_BYTES),
            ..WebSocketConfig::default()
        };
        let (socket, _) = tokio::select! {
            _ = cancellation.cancelled() => return Err(WebSocketError::Canceled),
            result = connect_async_with_config(request, Some(config), false) => result.map_err(|_| WebSocketError::Connect)?,
        };
        Ok(Self { socket })
    }

    pub(super) async fn send_text(
        &mut self,
        text: String,
        cancellation: &CancellationToken,
    ) -> Result<(), WebSocketError> {
        self.send(Message::Text(text), cancellation).await
    }

    pub(super) async fn send_binary(
        &mut self,
        bytes: Vec<u8>,
        cancellation: &CancellationToken,
    ) -> Result<(), WebSocketError> {
        self.send(Message::Binary(bytes), cancellation).await
    }

    pub(super) async fn receive(
        &mut self,
        cancellation: &CancellationToken,
    ) -> Result<Option<Message>, WebSocketError> {
        loop {
            let next = tokio::select! {
                _ = cancellation.cancelled() => return Err(WebSocketError::Canceled),
                message = self.socket.next() => message,
            };
            let Some(message) = next else {
                return Ok(None);
            };
            let message = message.map_err(|_| WebSocketError::Receive)?;
            match message {
                Message::Ping(payload) => self.send(Message::Pong(payload), cancellation).await?,
                Message::Pong(_) => {}
                message => return Ok(Some(message)),
            }
        }
    }

    pub(super) async fn close(&mut self) {
        // Teardown is best-effort and should not replace the session result.
        let _ = self.socket.close(None).await;
    }

    async fn send(
        &mut self,
        message: Message,
        cancellation: &CancellationToken,
    ) -> Result<(), WebSocketError> {
        tokio::select! {
            _ = cancellation.cancelled() => Err(WebSocketError::Canceled),
            result = self.socket.send(message) => result.map_err(|_| WebSocketError::Send),
        }
    }
}

pub(super) fn template_context<'a>(
    values: &'a BTreeMap<String, String>,
    secrets: &'a BTreeMap<String, String>,
    runtime: &'a RuntimeTemplateValues,
    chunk_base64: Option<String>,
) -> TemplateContext<'a> {
    // The temporary audio values need to live for the render call. Session
    // helpers construct their own local context when sending a chunk; this
    // connection context has no audio placeholder values.
    //
    // A leaked static empty map would be needless; callers only retain this
    // context within the current stack frame, and `Box::leak` is avoided by
    // using this helper only for connect fields (where audio is invalid).
    let _ = chunk_base64;
    static EMPTY_CAPTURES: std::sync::OnceLock<BTreeMap<String, String>> =
        std::sync::OnceLock::new();
    static EMPTY_AUDIO: std::sync::OnceLock<AudioTemplateValues> = std::sync::OnceLock::new();
    TemplateContext {
        values,
        secrets,
        captures: EMPTY_CAPTURES.get_or_init(BTreeMap::new),
        audio: EMPTY_AUDIO.get_or_init(AudioTemplateValues::default),
        runtime,
    }
}

pub(super) fn render(input: &str, context: &TemplateContext<'_>) -> Result<String, WebSocketError> {
    Template::parse(input)
        .and_then(|template| template.render(context))
        .map_err(WebSocketError::Template)
}

#[derive(Debug, Error)]
pub enum WebSocketError {
    #[error("realtime websocket URL is invalid")]
    InvalidUrl,
    #[error("realtime websocket header name is invalid")]
    InvalidHeaderName,
    #[error("realtime websocket header value is invalid")]
    InvalidHeaderValue,
    #[error("realtime websocket signer could not sign the connection")]
    Signing,
    #[error("realtime websocket connection failed")]
    Connect,
    #[error("realtime websocket send failed")]
    Send,
    #[error("realtime websocket receive failed")]
    Receive,
    #[error("realtime websocket session was canceled")]
    Canceled,
    #[error("realtime websocket template failed: {0}")]
    Template(#[source] TemplateError),
}
