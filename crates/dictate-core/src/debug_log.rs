//! Optional debug-log delivery for hosts without a console. Call sites keep the
//! existing configuration gates; without a subscriber, output goes to stderr.
use std::fmt;
use std::io::Write;
use std::sync::{Arc, Weak};
use std::time::SystemTime;

use parking_lot::RwLock;

const MAX_RECORD_BYTES: usize = 8192;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Ffmpeg,
    Record,
    Hotkey,
    Upload,
}

impl Category {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ffmpeg => "FFmpeg",
            Self::Record => "Record",
            Self::Hotkey => "Hotkey",
            Self::Upload => "Upload",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Record {
    pub timestamp: SystemTime,
    pub category: Category,
    pub message: String,
}

type Handler = dyn Fn(Record) + Send + Sync;

/// Keep this guard alive while the host consumes logs. Dropping it restores
/// stderr output once any callback already in progress has finished.
#[must_use]
pub struct Subscription {
    _handler: Arc<Handler>,
}

#[derive(Default)]
struct Dispatcher {
    handler: RwLock<Option<Weak<Handler>>>,
}

impl Dispatcher {
    fn subscribe(&self, handler: Arc<Handler>) -> Subscription {
        *self.handler.write() = Some(Arc::downgrade(&handler));
        Subscription { _handler: handler }
    }

    fn forward(&self, category: Category, message: &str) -> bool {
        let handler = self.handler.read().as_ref().and_then(Weak::upgrade);
        let Some(handler) = handler else {
            return false;
        };
        const SUFFIX: &str = "… [truncated]";
        let message = if message.len() > MAX_RECORD_BYTES {
            let mut end = MAX_RECORD_BYTES - SUFFIX.len();
            while !message.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}{SUFFIX}", &message[..end])
        } else {
            message.to_owned()
        };
        let record = Record {
            timestamp: SystemTime::now(),
            category,
            message,
        };
        // A host callback must not unwind into an audio worker or native callback.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(record)));
        true
    }
}

static DISPATCHER: Dispatcher = Dispatcher {
    handler: RwLock::new(None),
};

/// Installs the application's receiver. The callback should only enqueue logs;
/// it can run on recording, hotkey, HTTP, or FFmpeg worker threads.
pub fn subscribe(handler: impl Fn(Record) + Send + Sync + 'static) -> Subscription {
    DISPATCHER.subscribe(Arc::new(handler))
}

pub(crate) fn forward(category: Category, message: &str) -> bool {
    DISPATCHER.forward(category, message)
}

pub(crate) fn write(category: Category, message: fmt::Arguments<'_>) {
    let message = message.to_string();
    if !forward(category, &message) {
        // A detached GUI or a closed console must not abort a task.
        let _ = writeln!(std::io::stderr().lock(), "{message}");
    }
}

pub(crate) fn safe_url(input: &str) -> String {
    let Ok(mut url) = reqwest::Url::parse(input) else {
        return "<invalid URL>".into();
    };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    url.into()
}

/// Remove credentials before any caller truncates the message. Also cover JSON
/// escaping and URL encoding when a service echoes credentials in an error.
pub(crate) fn request_message(message: &str, config: &crate::Config) -> String {
    redact_request(message, config, None)
}

/// Include an in-flight prompt draft without replacing the saved credentials.
pub(crate) fn request_message_for_prompt(
    message: &str,
    config: &crate::Config,
    prompt: &crate::rewrite::RewritePrompt,
) -> String {
    redact_request(message, config, Some(prompt))
}

fn redact_request(
    message: &str,
    config: &crate::Config,
    prompt: Option<&crate::rewrite::RewritePrompt>,
) -> String {
    let prompts = || config.rewrite.prompts.iter().chain(prompt);
    let keys = [&config.token, &config.rewrite.api_key]
        .into_iter()
        .chain(prompts().map(|prompt| &prompt.api_key));
    let mut secrets: Vec<_> = keys
        .flat_map(|key| [key.clone(), key.trim().to_owned()])
        .collect();
    let urls = [&config.api_endpoint, &config.rewrite.base_url]
        .into_iter()
        .chain(prompts().map(|prompt| &prompt.base_url));
    for input in urls {
        if let Ok(url) = reqwest::Url::parse(input) {
            for secret in std::iter::once(url.username()).chain(url.password()) {
                secrets.push(secret.to_owned());
                // reqwest decodes URL userinfo before constructing Basic Auth;
                // a service may echo those decoded credentials in its response.
                if let Some(decoded) = decode_userinfo(secret) {
                    secrets.push(decoded);
                }
            }
            // Transport errors preserve the URL's original spelling. Decoding
            // then re-encoding loses lowercase escapes, encoded unreserved
            // characters, and mixed '+' / '%20' representations of spaces.
            if let Some(query) = url.query() {
                secrets.extend(
                    query
                        .split('&')
                        .filter_map(|pair| pair.split_once('=').map(|(_, value)| value.to_owned())),
                );
            }
            secrets.extend(url.query_pairs().map(|(_, value)| value.into_owned()));
        }
    }
    let mut variants = Vec::new();
    for secret in secrets.into_iter().filter(|secret| !secret.is_empty()) {
        let json = serde_json::to_string(&secret).expect("string serialization cannot fail");
        variants.push(json[1..json.len() - 1].to_owned());
        let mut url = reqwest::Url::parse("https://redact.invalid/").unwrap();
        url.query_pairs_mut().append_pair("key", &secret);
        if let Some(encoded) = url.query().and_then(|query| query.strip_prefix("key=")) {
            variants.push(encoded.to_owned());
            variants.push(encoded.replace('+', "%20"));
        }
        variants.push(secret);
    }
    let mut variants: Vec<_> = variants
        .into_iter()
        .map(|secret| normalize_percent_encoding(&secret))
        .collect();
    variants
        .sort_unstable_by(|left, right| right.len().cmp(&left.len()).then_with(|| left.cmp(right)));
    variants.dedup();
    variants
        .into_iter()
        .fold(message.to_owned(), |text, secret| {
            redact_variant(&text, &secret)
        })
}

// Userinfo uses percent encoding, not form encoding: '+' stays a literal plus.
// Match reqwest's one-pass decoding and strict UTF-8 handling. Invalid escapes
// remain literal; invalid UTF-8 has no decoded credential, so retain only raw.
fn decode_userinfo(input: &str) -> Option<String> {
    let encoded = input.as_bytes();
    let mut decoded = Vec::with_capacity(encoded.len());
    let mut index = 0;
    while index < encoded.len() {
        if encoded[index] == b'%'
            && index + 2 < encoded.len()
            && let Some(high) = (encoded[index + 1] as char).to_digit(16)
            && let Some(low) = (encoded[index + 2] as char).to_digit(16)
        {
            decoded.push((high * 16 + low) as u8);
            index += 3;
        } else {
            decoded.push(encoded[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

// Normalize only escape hex digits: credential letters remain case-sensitive,
// and byte offsets remain valid in the original UTF-8 message.
fn normalize_percent_encoding(input: &str) -> String {
    let mut bytes = input.as_bytes().to_vec();
    let mut index = 0;
    while index + 2 < bytes.len() {
        if bytes[index] == b'%'
            && bytes[index + 1].is_ascii_hexdigit()
            && bytes[index + 2].is_ascii_hexdigit()
        {
            bytes[index + 1].make_ascii_uppercase();
            bytes[index + 2].make_ascii_uppercase();
            index += 3;
        } else {
            index += 1;
        }
    }
    String::from_utf8(bytes).expect("changing ASCII hex digits preserves UTF-8")
}

fn redact_variant(message: &str, normalized_secret: &str) -> String {
    let normalized_message = normalize_percent_encoding(message);
    let mut output = String::with_capacity(message.len());
    let mut end = 0;
    for (start, matched) in normalized_message.match_indices(normalized_secret) {
        output.push_str(&message[end..start]);
        output.push_str("[redacted]");
        end = start + matched.len();
    }
    output.push_str(&message[end..]);
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;

    #[test]
    fn receiver_lifetime_and_replacement_restore_fallback() {
        let dispatcher = Dispatcher::default();
        assert!(!dispatcher.forward(Category::Upload, "before subscription"));
        let records = Arc::new(Mutex::new(Vec::new()));
        let output = records.clone();
        let first = dispatcher.subscribe(Arc::new(move |entry| output.lock().push(entry)));
        assert!(dispatcher.forward(Category::Record, "audio"));
        assert_eq!(records.lock()[0].category, Category::Record);
        assert_eq!(records.lock()[0].message, "audio");
        let second = dispatcher.subscribe(Arc::new(|_| {}));
        drop(first);
        assert!(dispatcher.forward(Category::Hotkey, "new receiver"));
        assert_eq!(records.lock().len(), 1);
        drop(second);
        assert!(!dispatcher.forward(Category::Ffmpeg, "stderr again"));
    }

    #[test]
    fn callbacks_can_reenter_without_holding_dispatch_lock_and_are_bounded() {
        let dispatcher = Arc::new(Dispatcher::default());
        let callback_dispatcher = dispatcher.clone();
        let guard = dispatcher.subscribe(Arc::new(move |entry| {
            assert!(entry.message.len() <= MAX_RECORD_BYTES);
            assert!(entry.message.is_char_boundary(entry.message.len()));
            let _guard = callback_dispatcher.subscribe(Arc::new(|_| {}));
        }));
        assert!(dispatcher.forward(Category::Upload, &"文本".repeat(MAX_RECORD_BYTES)));
        drop(guard);
    }

    #[test]
    fn host_panics_do_not_escape_the_log_dispatcher() {
        let dispatcher = Dispatcher::default();
        let _guard = dispatcher.subscribe(Arc::new(|_| panic!("host failure")));
        assert!(dispatcher.forward(Category::Record, "worker stays alive"));
    }

    #[test]
    fn request_credentials_are_removed_before_bounded_display() {
        let mut config = crate::Config {
            token: "key\" with space".into(),
            api_endpoint: "https://alice:password@example.com/v1?token=query-secret".into(),
            ..Default::default()
        };
        config.rewrite.api_key = "rewrite-secret".into();
        let text = r#"key\" with space key%22+with+space key%22%20with%20space rewrite-secret alice password query-secret"#;
        let clean = request_message(text, &config);
        for secret in ["key", "rewrite-secret", "alice", "password", "query-secret"] {
            assert!(!clean.contains(secret), "{clean}");
        }
        assert_eq!(safe_url(&config.api_endpoint), "https://example.com/v1");
        assert_eq!(safe_url("bad credentials"), "<invalid URL>");
    }

    #[test]
    fn request_credentials_cover_original_query_encoding_and_escape_case() {
        for (encoded, decoded, equivalent) in [
            ("secret%2fvalue", "secret/value", "secret%2Fvalue"),
            ("mix%2fed%2Fsecret", "mix/ed/secret", "mix%2Fed%2fsecret"),
            ("%73%65cret", "secret", "%73%65cret"),
            (
                "secret+with%20space",
                "secret with space",
                "secret+with+space",
            ),
            ("secret%2bvalue", "secret+value", "secret%2Bvalue"),
            ("secret%22value", "secret\"value", "secret%22value"),
            ("secret%e6%96%87", "secret文", "secret%E6%96%87"),
        ] {
            let mut config = crate::Config::default();
            for rewrite in [false, true] {
                if rewrite {
                    config.api_endpoint.clear();
                    config.rewrite.base_url = format!("https://example.com/?api_key={encoded}");
                } else {
                    config.api_endpoint = format!("https://example.com/?api_key={encoded}");
                }
                let json = serde_json::to_string(decoded).unwrap();
                let mut url = reqwest::Url::parse("https://example.com/").unwrap();
                url.query_pairs_mut().append_pair("key", decoded);
                let form = url.query().unwrap().strip_prefix("key=").unwrap();
                for spelling in [encoded, decoded, &json[1..json.len() - 1], form, equivalent] {
                    assert_eq!(
                        request_message(&format!("错误：{spelling}; intact=%2f"), &config),
                        "错误：[redacted]; intact=%2f",
                        "encoded={encoded}, spelling={spelling}, rewrite={rewrite}"
                    );
                }
                assert_eq!(
                    request_message(&form.replace('+', "%20"), &config),
                    "[redacted]"
                );
            }
        }
    }

    #[test]
    fn redaction_keeps_credential_letters_case_sensitive_and_other_text_unchanged() {
        let config = crate::Config {
            token: "Secret/Key".into(),
            ..Default::default()
        };
        assert_eq!(
            request_message("原文 secret%2fKey Secret%2fKey other=%2f", &config),
            "原文 secret%2fKey [redacted] other=%2f"
        );
    }

    #[test]
    fn encoded_userinfo_redacts_raw_decoded_json_and_url_spellings() {
        for (username, password, decoded_username, decoded_password) in [
            ("user%2fname", "pass%40word", "user/name", "pass@word"),
            ("user+name", "pass+word", "user+name", "pass+word"),
            ("user%2Bname", "pass%2bword", "user+name", "pass+word"),
            ("%E7%94%A8%e6%88%b7", "%e5%af%86%E7%A0%81", "用户", "密码"),
            (
                "user%22name",
                "pass%5Cword%0Aline",
                "user\"name",
                "pass\\word\nline",
            ),
            (
                "user%252Fname",
                "pass%252Bword",
                "user%2Fname",
                "pass%2Bword",
            ),
            ("user%2name", "pass%GGtail%", "user%2name", "pass%GGtail%"),
        ] {
            for rewrite in [false, true] {
                let mut config = crate::Config::default();
                let url = format!("https://{username}:{password}@example.com/");
                if rewrite {
                    config.rewrite.base_url = url;
                } else {
                    config.api_endpoint = url;
                }
                for (encoded, decoded) in
                    [(username, decoded_username), (password, decoded_password)]
                {
                    let json = serde_json::to_string(decoded).unwrap();
                    let mut url = reqwest::Url::parse("https://example.com/").unwrap();
                    url.query_pairs_mut().append_pair("key", decoded);
                    let form = url.query().unwrap().strip_prefix("key=").unwrap();
                    for spelling in [encoded, decoded, &json[1..json.len() - 1], form] {
                        assert_eq!(
                            request_message(&format!("错误：{spelling}; unchanged=%2f"), &config),
                            "错误：[redacted]; unchanged=%2f",
                            "username={username}, password={password}, rewrite={rewrite}"
                        );
                    }
                    assert_eq!(
                        request_message(&form.replace('+', "%20"), &config),
                        "[redacted]"
                    );
                }
            }
        }
    }

    #[test]
    fn userinfo_decoding_preserves_plus_case_and_invalid_utf8_without_decoding_twice() {
        let mut config = crate::Config {
            api_endpoint: "https://User+name:Pass%2bword@example.com/".into(),
            ..Default::default()
        };
        assert_eq!(
            request_message(
                "User+name Pass+word; User name Pass word user+name pass+word",
                &config
            ),
            "[redacted] [redacted]; User name Pass word user+name pass+word"
        );
        config.api_endpoint = "https://user%252Fname:pass%252Bword@example.com/".into();
        assert_eq!(
            request_message("user%2Fname pass%2Bword; user/name pass+word", &config),
            "[redacted] [redacted]; user/name pass+word"
        );
        config.api_endpoint = "https://user%FF:pass%fe@example.com/".into();
        assert_eq!(
            request_message("user%FF pass%fe; unchanged=�", &config),
            "[redacted] [redacted]; unchanged=�"
        );
    }

    #[test]
    fn saved_prompt_keys_and_url_credentials_are_redacted_for_all_requests() {
        let mut config = crate::Config {
            token: "audio-global-key".into(),
            ..Default::default()
        };
        config.rewrite.api_key = "rewrite-global-key".into();
        config.rewrite.prompts = vec![
            crate::rewrite::RewritePrompt {
                api_key: " prompt\"saved-key ".into(),
                base_url: "https://prompt%2buser:prompt%2Fpass@example.com/?token=prompt%2fquery"
                    .into(),
                ..Default::default()
            },
            crate::rewrite::RewritePrompt {
                api_key: "other-prompt-key".into(),
                base_url: "https://example.com/?token=other+query%20key".into(),
                ..Default::default()
            },
        ];
        for secret in [
            "audio-global-key",
            "rewrite-global-key",
            " prompt\"saved-key ",
            "prompt\"saved-key",
            "prompt\\\"saved-key",
            "prompt%22saved-key",
            "other-prompt-key",
            "prompt%2buser",
            "prompt+user",
            "prompt%2Fpass",
            "prompt/pass",
            "prompt%2fquery",
            "prompt/query",
            "other+query%20key",
            "other query key",
        ] {
            assert_eq!(
                request_message(&format!("echo={secret}; intact"), &config),
                "echo=[redacted]; intact",
                "secret={secret}"
            );
        }
    }

    #[test]
    fn prompt_draft_redaction_keeps_saved_credentials_and_does_not_mutate_config() {
        let mut config = crate::Config::default();
        config.rewrite.api_key = "main-key".into();
        config.rewrite.prompts = vec![
            crate::rewrite::RewritePrompt {
                api_key: "original-key".into(),
                ..Default::default()
            },
            crate::rewrite::RewritePrompt {
                api_key: "another-key".into(),
                ..Default::default()
            },
        ];
        let mut draft = config.rewrite.prompts[0].clone();
        draft.api_key = "  draft\"key  ".into();
        draft.base_url =
            "https://draft%2Buser:draft%2Fpass@example.com/?token=draft%2Fquery".into();
        for secret in [
            "main-key",
            "original-key",
            "another-key",
            "  draft\"key  ",
            "draft\"key",
            "draft\\\"key",
            "draft%22key",
            "draft%2Buser",
            "draft+user",
            "draft%2Fpass",
            "draft/pass",
            "draft%2Fquery",
            "draft/query",
        ] {
            assert_eq!(
                request_message_for_prompt(&format!("echo={secret}; intact"), &config, &draft),
                "echo=[redacted]; intact",
                "secret={secret}"
            );
        }
        assert_eq!(request_message("draft\"key", &config), "draft\"key");
        assert_eq!(config.rewrite.prompts[0].api_key, "original-key");
    }
}
