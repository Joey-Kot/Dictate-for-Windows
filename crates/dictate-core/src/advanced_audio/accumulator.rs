//! Shared transcript assembly for streaming recognition protocols.
//!
//! The accumulator deliberately knows nothing about a vendor wire format or a
//! workflow schema. A transport maps its events to [`TranscriptAction`] and
//! feeds them here. This keeps the same completion and final-text semantics
//! usable by HTTP response streams and realtime sessions.

use std::error::Error;
use std::fmt;

/// A normalized transcript event accepted by [`TranscriptAccumulator`].
///
/// These are intentionally the only state-changing actions supported by the
/// accumulator. In particular, an upstream protocol must distinguish an
/// additive delta from a replacement partial hypothesis before constructing an
/// action; treating both as deltas duplicates text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptAction {
    /// Ignore a protocol event which has no transcript meaning.
    Ignore,
    /// Append a genuine incremental token to the current partial hypothesis.
    AppendDelta(String),
    /// Replace the current revisable partial hypothesis.
    ReplacePartial(String),
    /// Add a stable segment and discard the current partial hypothesis.
    CommitSegment(String),
    /// Set the complete transcript supplied by the server.
    ///
    /// A final text is authoritative and takes precedence over accumulated
    /// segments and partial text once the stream completes.
    SetFinalText(String),
    /// Mark the stream as successfully complete.
    Complete,
    /// Mark the stream as failed with the server-provided reason.
    Fail(String),
}

impl TranscriptAction {
    /// Returns the stable workflow action spelling for this action.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Ignore => "ignore",
            Self::AppendDelta(_) => "append_delta",
            Self::ReplacePartial(_) => "replace_partial",
            Self::CommitSegment(_) => "commit_segment",
            Self::SetFinalText(_) => "set_final_text",
            Self::Complete => "complete",
            Self::Fail(_) => "fail",
        }
    }

    /// Builds an [`AppendDelta`](Self::AppendDelta) action.
    pub fn append_delta(text: impl Into<String>) -> Self {
        Self::AppendDelta(text.into())
    }

    /// Builds a [`ReplacePartial`](Self::ReplacePartial) action.
    pub fn replace_partial(text: impl Into<String>) -> Self {
        Self::ReplacePartial(text.into())
    }

    /// Builds a [`CommitSegment`](Self::CommitSegment) action.
    pub fn commit_segment(text: impl Into<String>) -> Self {
        Self::CommitSegment(text.into())
    }

    /// Builds a [`SetFinalText`](Self::SetFinalText) action.
    pub fn set_final_text(text: impl Into<String>) -> Self {
        Self::SetFinalText(text.into())
    }

    /// Builds a [`Fail`](Self::Fail) action.
    pub fn fail(message: impl Into<String>) -> Self {
        Self::Fail(message.into())
    }
}

/// The terminal status of a [`TranscriptAccumulator`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptAccumulatorState {
    /// More stream events may be applied.
    Open,
    /// A `complete` action was received.
    Complete,
    /// A `fail` action was received.
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    Open,
    Complete,
    Failed(String),
}

impl State {
    const fn public_state(&self) -> TranscriptAccumulatorState {
        match self {
            Self::Open => TranscriptAccumulatorState::Open,
            Self::Complete => TranscriptAccumulatorState::Complete,
            Self::Failed(_) => TranscriptAccumulatorState::Failed,
        }
    }
}

/// Errors returned while applying transcript events or reading a final result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptAccumulatorError {
    /// The caller requested a final transcript before an explicit `complete`.
    IncompleteStream,
    /// The server explicitly failed the stream.
    StreamFailed { message: String },
    /// An action was received after normal completion.
    ActionAfterComplete { action: &'static str },
    /// An action was received after the server failed the stream.
    ActionAfterFailure {
        action: &'static str,
        message: String,
    },
}

impl fmt::Display for TranscriptAccumulatorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IncompleteStream => formatter.write_str(
                "cannot return transcript: stream ended without an explicit `complete` action",
            ),
            Self::StreamFailed { message } => write!(formatter, "stream failed: {message}"),
            Self::ActionAfterComplete { action } => write!(
                formatter,
                "cannot apply `{action}`: stream is already complete"
            ),
            Self::ActionAfterFailure { action, message } => write!(
                formatter,
                "cannot apply `{action}`: stream has already failed: {message}"
            ),
        }
    }
}

impl Error for TranscriptAccumulatorError {}

/// Collects normalized streaming transcript events into one final transcript.
///
/// Text is concatenated exactly as supplied; no whitespace or separators are
/// invented. This is important because APIs vary in whether segments include
/// their own surrounding spaces. [`Self::final_text`] refuses to return text
/// until a [`TranscriptAction::Complete`] action has been applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptAccumulator {
    committed: String,
    partial: String,
    authoritative_final: Option<String>,
    state: State,
}

impl Default for TranscriptAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

impl TranscriptAccumulator {
    /// Creates an empty, open accumulator.
    pub fn new() -> Self {
        Self {
            committed: String::new(),
            partial: String::new(),
            authoritative_final: None,
            state: State::Open,
        }
    }

    /// Applies one normalized event.
    ///
    /// `Fail` records the failure and returns it immediately. All actions are
    /// rejected after either terminal action, making accidental use of stale
    /// stream data visible to the caller.
    pub fn apply(&mut self, action: TranscriptAction) -> Result<(), TranscriptAccumulatorError> {
        self.ensure_open(action.name())?;

        match action {
            TranscriptAction::Ignore => Ok(()),
            TranscriptAction::AppendDelta(text) => {
                self.partial.push_str(&text);
                Ok(())
            }
            TranscriptAction::ReplacePartial(text) => {
                self.partial = text;
                Ok(())
            }
            TranscriptAction::CommitSegment(text) => {
                self.committed.push_str(&text);
                self.partial.clear();
                Ok(())
            }
            TranscriptAction::SetFinalText(text) => {
                self.authoritative_final = Some(text);
                Ok(())
            }
            TranscriptAction::Complete => {
                self.state = State::Complete;
                Ok(())
            }
            TranscriptAction::Fail(message) => {
                let message = nonempty_failure_message(message);
                self.state = State::Failed(message.clone());
                Err(TranscriptAccumulatorError::StreamFailed { message })
            }
        }
    }

    /// Applies an `append_delta` action.
    pub fn append_delta(
        &mut self,
        text: impl Into<String>,
    ) -> Result<(), TranscriptAccumulatorError> {
        self.apply(TranscriptAction::append_delta(text))
    }

    /// Applies a `replace_partial` action.
    pub fn replace_partial(
        &mut self,
        text: impl Into<String>,
    ) -> Result<(), TranscriptAccumulatorError> {
        self.apply(TranscriptAction::replace_partial(text))
    }

    /// Applies a `commit_segment` action.
    pub fn commit_segment(
        &mut self,
        text: impl Into<String>,
    ) -> Result<(), TranscriptAccumulatorError> {
        self.apply(TranscriptAction::commit_segment(text))
    }

    /// Applies a `set_final_text` action.
    pub fn set_final_text(
        &mut self,
        text: impl Into<String>,
    ) -> Result<(), TranscriptAccumulatorError> {
        self.apply(TranscriptAction::set_final_text(text))
    }

    /// Applies a `complete` action.
    pub fn complete(&mut self) -> Result<(), TranscriptAccumulatorError> {
        self.apply(TranscriptAction::Complete)
    }

    /// Applies a `fail` action and returns the resulting stream error.
    pub fn fail(&mut self, message: impl Into<String>) -> Result<(), TranscriptAccumulatorError> {
        self.apply(TranscriptAction::fail(message))
    }

    /// Returns the completed transcript.
    ///
    /// An authoritative final text wins over accumulated segments and partial
    /// text. A stream without `complete` is deliberately rejected even if it
    /// has received a final-text event.
    pub fn final_text(&self) -> Result<String, TranscriptAccumulatorError> {
        match &self.state {
            State::Open => Err(TranscriptAccumulatorError::IncompleteStream),
            State::Failed(message) => Err(TranscriptAccumulatorError::StreamFailed {
                message: message.clone(),
            }),
            State::Complete => Ok(self
                .authoritative_final
                .clone()
                .unwrap_or_else(|| self.accumulated_text())),
        }
    }

    /// Consumes the accumulator and returns the completed transcript.
    pub fn into_final_text(self) -> Result<String, TranscriptAccumulatorError> {
        match self.state {
            State::Open => Err(TranscriptAccumulatorError::IncompleteStream),
            State::Failed(message) => Err(TranscriptAccumulatorError::StreamFailed { message }),
            State::Complete => Ok(self.authoritative_final.unwrap_or_else(|| {
                let mut text = self.committed;
                text.push_str(&self.partial);
                text
            })),
        }
    }

    /// Returns the current lifecycle state.
    pub const fn state(&self) -> TranscriptAccumulatorState {
        self.state.public_state()
    }

    /// Returns whether `complete` was received.
    pub const fn is_complete(&self) -> bool {
        matches!(self.state, State::Complete)
    }

    /// Returns the server's failure message after a `fail` action.
    pub fn failure_message(&self) -> Option<&str> {
        match &self.state {
            State::Failed(message) => Some(message),
            State::Open | State::Complete => None,
        }
    }

    /// Returns committed stable text, excluding any current partial.
    pub fn committed_text(&self) -> &str {
        &self.committed
    }

    /// Returns the current revisable partial hypothesis.
    pub fn partial_text(&self) -> &str {
        &self.partial
    }

    /// Returns the authoritative server final, if one has been received.
    pub fn authoritative_final_text(&self) -> Option<&str> {
        self.authoritative_final.as_deref()
    }

    fn ensure_open(&self, action: &'static str) -> Result<(), TranscriptAccumulatorError> {
        match &self.state {
            State::Open => Ok(()),
            State::Complete => Err(TranscriptAccumulatorError::ActionAfterComplete { action }),
            State::Failed(message) => Err(TranscriptAccumulatorError::ActionAfterFailure {
                action,
                message: message.clone(),
            }),
        }
    }

    fn accumulated_text(&self) -> String {
        let mut text = self.committed.clone();
        text.push_str(&self.partial);
        text
    }
}

fn nonempty_failure_message(message: String) -> String {
    if message.trim().is_empty() {
        "server reported stream failure without a message".into()
    } else {
        message
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_delta_builds_one_partial_hypothesis() {
        let mut accumulator = TranscriptAccumulator::new();

        accumulator.append_delta("hel").unwrap();
        accumulator.append_delta("lo").unwrap();

        assert_eq!(accumulator.partial_text(), "hello");
        assert_eq!(accumulator.committed_text(), "");
        accumulator.complete().unwrap();
        assert_eq!(accumulator.final_text().unwrap(), "hello");
    }

    #[test]
    fn replacement_does_not_append_old_partial_hypotheses() {
        let mut accumulator = TranscriptAccumulator::new();

        accumulator.replace_partial("hello wor").unwrap();
        accumulator.replace_partial("hello world").unwrap();
        accumulator.complete().unwrap();

        assert_eq!(accumulator.final_text().unwrap(), "hello world");
    }

    #[test]
    fn commits_stable_segments_and_clears_the_partial() {
        let mut accumulator = TranscriptAccumulator::new();

        accumulator.replace_partial("first provisional").unwrap();
        accumulator.commit_segment("first ").unwrap();
        accumulator.append_delta("sec").unwrap();
        accumulator.replace_partial("second").unwrap();
        accumulator.commit_segment("second ").unwrap();
        accumulator.append_delta("third").unwrap();
        accumulator.complete().unwrap();

        assert_eq!(accumulator.committed_text(), "first second ");
        assert_eq!(accumulator.partial_text(), "third");
        assert_eq!(accumulator.final_text().unwrap(), "first second third");
    }

    #[test]
    fn authoritative_final_overrides_segments_and_partial() {
        let mut accumulator = TranscriptAccumulator::new();

        accumulator.commit_segment("stale ").unwrap();
        accumulator.replace_partial("partial").unwrap();
        accumulator.set_final_text("authoritative result").unwrap();
        accumulator.complete().unwrap();

        assert_eq!(accumulator.final_text().unwrap(), "authoritative result");
    }

    #[test]
    fn final_text_requires_an_explicit_completion_event() {
        let mut accumulator = TranscriptAccumulator::new();
        accumulator.set_final_text("looks complete").unwrap();

        assert_eq!(
            accumulator.final_text(),
            Err(TranscriptAccumulatorError::IncompleteStream)
        );
        assert_eq!(
            accumulator.final_text().unwrap_err().to_string(),
            "cannot return transcript: stream ended without an explicit `complete` action"
        );
    }

    #[test]
    fn server_failure_is_immediate_and_prevents_output() {
        let mut accumulator = TranscriptAccumulator::new();
        accumulator.append_delta("partial").unwrap();

        assert_eq!(
            accumulator.fail("quota exceeded"),
            Err(TranscriptAccumulatorError::StreamFailed {
                message: "quota exceeded".into(),
            })
        );
        assert_eq!(accumulator.state(), TranscriptAccumulatorState::Failed);
        assert_eq!(accumulator.failure_message(), Some("quota exceeded"));
        assert_eq!(
            accumulator.final_text(),
            Err(TranscriptAccumulatorError::StreamFailed {
                message: "quota exceeded".into(),
            })
        );
    }

    #[test]
    fn empty_server_failure_still_has_a_clear_reason() {
        let mut accumulator = TranscriptAccumulator::new();

        let error = accumulator.fail("   ").unwrap_err();

        assert_eq!(
            error.to_string(),
            "stream failed: server reported stream failure without a message"
        );
        assert_eq!(
            accumulator.failure_message(),
            Some("server reported stream failure without a message")
        );
    }

    #[test]
    fn actions_after_a_terminal_event_are_rejected_with_the_action_name() {
        let mut completed = TranscriptAccumulator::new();
        completed.complete().unwrap();
        assert_eq!(
            completed.append_delta("late"),
            Err(TranscriptAccumulatorError::ActionAfterComplete {
                action: "append_delta",
            })
        );

        let mut failed = TranscriptAccumulator::new();
        let _ = failed.fail("upstream error");
        assert_eq!(
            failed.complete(),
            Err(TranscriptAccumulatorError::ActionAfterFailure {
                action: "complete",
                message: "upstream error".into(),
            })
        );
    }

    #[test]
    fn ignore_has_no_transcript_effect() {
        let mut accumulator = TranscriptAccumulator::new();

        accumulator.apply(TranscriptAction::Ignore).unwrap();
        accumulator.append_delta("text").unwrap();
        accumulator.complete().unwrap();

        assert_eq!(accumulator.final_text().unwrap(), "text");
    }

    #[test]
    fn action_names_match_workflow_action_spelling() {
        let actions = [
            (TranscriptAction::Ignore, "ignore"),
            (TranscriptAction::append_delta("a"), "append_delta"),
            (TranscriptAction::replace_partial("a"), "replace_partial"),
            (TranscriptAction::commit_segment("a"), "commit_segment"),
            (TranscriptAction::set_final_text("a"), "set_final_text"),
            (TranscriptAction::Complete, "complete"),
            (TranscriptAction::fail("error"), "fail"),
        ];

        for (action, expected) in actions {
            assert_eq!(action.name(), expected);
        }
    }

    #[test]
    fn an_empty_authoritative_final_is_preserved() {
        let mut accumulator = TranscriptAccumulator::new();

        accumulator.append_delta("non-final text").unwrap();
        accumulator.set_final_text("").unwrap();
        accumulator.complete().unwrap();

        assert_eq!(accumulator.final_text().unwrap(), "");
    }

    #[test]
    fn owned_finalization_uses_accumulated_text() {
        let mut accumulator = TranscriptAccumulator::new();
        accumulator.commit_segment("one ").unwrap();
        accumulator.replace_partial("two").unwrap();
        accumulator.complete().unwrap();

        assert_eq!(accumulator.into_final_text().unwrap(), "one two");
    }
}
