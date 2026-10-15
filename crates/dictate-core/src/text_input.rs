//! Shared text output. Selected channels never fall back or retry delivery.
use std::time::Duration;

use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    clipboard::{self, ClipboardError},
    config::Config,
};

#[derive(Debug, Error)]
pub enum TextInputError {
    #[error(transparent)]
    Clipboard(#[from] ClipboardError),
    #[error("input canceled after {sent} events; injected text cannot be recalled")]
    Canceled { sent: usize },
    #[error("modifier keys remained pressed for 2 seconds")]
    ModifiersHeld,
    #[error("SendInput inserted {sent} of {total} events; system error: {code}")]
    Send {
        sent: usize,
        total: usize,
        code: u32,
    },
    #[error("Unicode input is only available on Windows")]
    UnsupportedPlatform,
}

impl TextInputError {
    pub fn canceled_before_output(&self) -> bool {
        matches!(
            self,
            Self::Clipboard(ClipboardError::Canceled) | Self::Canceled { sent: 0 }
        )
    }

    pub fn status(&self) -> &'static str {
        match self {
            Self::Clipboard(error) if error.paste_was_sent_before_restore_failure() => {
                "Paste sent; clipboard restore failed"
            }
            Self::Clipboard(_) => "Paste failed",
            Self::Canceled { .. } => "Input canceled; text may already be present",
            Self::Send { sent, .. } if *sent > 0 => "Input incomplete; text may already be present",
            _ => "Text input failed",
        }
    }
}

pub async fn send_text(
    text: &str,
    cancellation: &CancellationToken,
    config: &Config,
) -> Result<(), TextInputError> {
    if config.use_sendinput {
        send_unicode(text, cancellation, config.sendinput_batch_size as usize).await
    } else {
        clipboard::paste_text(
            text,
            cancellation,
            Duration::from_millis(config.clipboard_write_delay),
            Duration::from_millis(config.clipboard_restore_delay),
        )
        .await
        .map_err(Into::into)
    }
}

// Keep each scalar, including surrogate pairs, in one batch.
#[cfg(any(windows, test))]
fn unicode_batches(text: &str, batch_size: usize) -> Vec<Vec<u16>> {
    let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
    let mut batches = Vec::new();
    let mut batch = Vec::new();
    for ch in normalized.chars() {
        // A scalar can use two UTF-16 units; retain it intact even if the
        // configured limit is one unit.
        if !batch.is_empty() && batch.len() + ch.len_utf16() > batch_size {
            batches.push(std::mem::take(&mut batch));
        }
        batch.extend_from_slice(ch.encode_utf16(&mut [0; 2]));
    }
    if !batch.is_empty() {
        batches.push(batch);
    }
    batches
}

#[cfg(any(windows, test))]
trait UnicodeBackend {
    fn modifiers_pressed(&self) -> bool;
    fn send(&self, units: &[u16]) -> (usize, u32);
}

#[cfg(any(windows, test))]
async fn send_unicode_with(
    text: &str,
    cancellation: &CancellationToken,
    backend: &impl UnicodeBackend,
    batch_size: usize,
) -> Result<(), TextInputError> {
    let batches = unicode_batches(text, batch_size.max(1));
    let total = batches.iter().map(|batch| batch.len() * 2).sum();
    let mut sent = 0;
    for batch in batches {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            if cancellation.is_cancelled() {
                return Err(TextInputError::Canceled { sent });
            }
            if !backend.modifiers_pressed() {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                return if sent == 0 {
                    Err(TextInputError::ModifiersHeld)
                } else {
                    Err(TextInputError::Send {
                        sent,
                        total,
                        code: 0,
                    })
                };
            }
            tokio::select! {
                _ = cancellation.cancelled() => return Err(TextInputError::Canceled { sent }),
                _ = tokio::time::sleep(Duration::from_millis(10)) => {}
            }
        }
        let (inserted, code) = backend.send(&batch);
        sent += inserted;
        if inserted != batch.len() * 2 {
            return Err(TextInputError::Send { sent, total, code });
        }
        tokio::task::yield_now().await;
    }
    Ok(())
}

async fn send_unicode(
    text: &str,
    cancellation: &CancellationToken,
    batch_size: usize,
) -> Result<(), TextInputError> {
    #[cfg(windows)]
    {
        send_unicode_with(
            text,
            cancellation,
            &crate::keyboard::WindowsUnicode,
            batch_size,
        )
        .await
    }
    #[cfg(not(windows))]
    {
        let _ = (text, cancellation, batch_size);
        Err(TextInputError::UnsupportedPlatform)
    }
}

#[cfg(windows)]
impl UnicodeBackend for crate::keyboard::WindowsUnicode {
    fn modifiers_pressed(&self) -> bool {
        self.modifiers_pressed()
    }
    fn send(&self, units: &[u16]) -> (usize, u32) {
        self.send(units)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[cfg(not(windows))]
    #[tokio::test]
    async fn selected_channel_error_is_not_replaced_by_fallback() {
        let mut config = Config::default();
        let token = CancellationToken::new();
        assert!(matches!(
            send_text("text", &token, &config).await,
            Err(TextInputError::Clipboard(
                ClipboardError::UnsupportedPlatform
            ))
        ));
        config.use_sendinput = true;
        assert!(matches!(
            send_text("text", &token, &config).await,
            Err(TextInputError::UnsupportedPlatform)
        ));
    }

    #[tokio::test]
    async fn complete_send_and_empty_text() {
        let backend = Backend {
            calls: Cell::new(0),
            limit: usize::MAX,
            cancel: None,
        };
        let token = CancellationToken::new();
        send_unicode_with("", &token, &backend, 128).await.unwrap();
        assert_eq!(backend.calls.get(), 0);
        send_unicode_with(&"a".repeat(300), &token, &backend, 128)
            .await
            .unwrap();
        assert_eq!(backend.calls.get(), 3);

        let larger_batch = Backend {
            calls: Cell::new(0),
            limit: usize::MAX,
            cancel: None,
        };
        send_unicode_with(&"a".repeat(300), &token, &larger_batch, 1024)
            .await
            .unwrap();
        assert_eq!(larger_batch.calls.get(), 1);
    }

    #[tokio::test]
    async fn held_modifiers_allow_cancellation_without_injection() {
        struct Held;
        impl UnicodeBackend for Held {
            fn modifiers_pressed(&self) -> bool {
                true
            }
            fn send(&self, _: &[u16]) -> (usize, u32) {
                panic!("must wait for release")
            }
        }
        let token = CancellationToken::new();
        let cancel = async {
            tokio::task::yield_now().await;
            token.cancel();
        };
        let (result, ()) = tokio::join!(send_unicode_with("abc", &token, &Held, 128), cancel);
        assert!(matches!(result, Err(TextInputError::Canceled { sent: 0 })));
    }

    struct Backend {
        calls: Cell<usize>,
        limit: usize,
        cancel: Option<CancellationToken>,
    }
    impl UnicodeBackend for Backend {
        fn modifiers_pressed(&self) -> bool {
            false
        }
        fn send(&self, units: &[u16]) -> (usize, u32) {
            self.calls.set(self.calls.get() + 1);
            if let Some(token) = &self.cancel {
                token.cancel();
            }
            ((units.len() * 2).min(self.limit), 0)
        }
    }
    #[test]
    fn encoding_and_batch_boundaries() {
        assert!(unicode_batches("", 128).is_empty());
        let text = format!("{}😀中\r\n\n\r\t", "a".repeat(127));
        let batches = unicode_batches(&text, 128);
        assert_eq!(batches[0].len(), 127);
        assert_eq!(
            String::from_utf16(&batches.concat()).unwrap(),
            format!("{}😀中\r\r\r\t", "a".repeat(127))
        );
        for batch in batches {
            assert!(String::from_utf16(&batch).is_ok());
        }
        assert_eq!(
            unicode_batches("😀", 1),
            vec!["😀".encode_utf16().collect::<Vec<_>>()]
        );
    }
    #[tokio::test]
    async fn partial_or_zero_send_stops_without_retry() {
        for limit in [0, 1, 3] {
            let backend = Backend {
                calls: Cell::new(0),
                limit,
                cancel: None,
            };
            assert!(
                matches!(send_unicode_with(&"中".repeat(300), &CancellationToken::new(), &backend, 128).await, Err(TextInputError::Send { sent, .. }) if sent == limit)
            );
            assert_eq!(backend.calls.get(), 1);
        }
    }
    #[tokio::test]
    async fn cancellation_stops_remaining_batches() {
        let token = CancellationToken::new();
        let backend = Backend {
            calls: Cell::new(0),
            limit: usize::MAX,
            cancel: Some(token.clone()),
        };
        assert!(matches!(
            send_unicode_with(&"a".repeat(300), &token, &backend, 128).await,
            Err(TextInputError::Canceled { sent: 256 })
        ));
        assert_eq!(backend.calls.get(), 1);
        assert!(matches!(
            send_unicode_with("abc", &token, &backend, 128).await,
            Err(TextInputError::Canceled { sent: 0 })
        ));
        assert_eq!(backend.calls.get(), 1);
    }
}
