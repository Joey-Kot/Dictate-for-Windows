use super::*;
use std::cell::RefCell;

const WRITE_DELAY: Duration = Duration::from_millis(80);
const RESTORE_DELAY: Duration = Duration::from_millis(120);

#[derive(Clone, Debug, PartialEq, Eq)]
struct Clipboard {
    text: Option<String>,
    custom_data: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Event {
    Backup,
    Copy,
    Copied,
    Restore,
}

struct State {
    elapsed: Duration,
    clipboard: Clipboard,
    sequence: u32,
    events: Vec<Event>,
    event_times: Vec<(Event, Duration)>,
    payload: Option<String>,
    copy_delay: Duration,
    copy_due: Option<Duration>,
    keys_until: Duration,
    focus_until: Option<Duration>,
    cancel_at: Option<Duration>,
    cancel_on_backup: bool,
    backup_fails: bool,
    injection_fails: bool,
    read_fails: bool,
    restore_fails: bool,
}

impl State {
    fn record(&mut self, event: Event) {
        self.events.push(event);
        self.event_times.push((event, self.elapsed));
    }

    fn event_time(&self, event: Event) -> Duration {
        self.event_times
            .iter()
            .find_map(|&(recorded, time)| (recorded == event).then_some(time))
            .expect("event should have occurred")
    }
}

struct FakeCopy {
    start: Instant,
    cancel: CancellationToken,
    original: Clipboard,
    state: RefCell<State>,
}

impl FakeCopy {
    fn new(payload: Option<&str>) -> Self {
        let original = Clipboard {
            text: Some("original clipboard".into()),
            custom_data: vec![0, 255, 42],
        };
        Self {
            start: Instant::now(),
            cancel: CancellationToken::new(),
            original: original.clone(),
            state: RefCell::new(State {
                elapsed: Duration::ZERO,
                clipboard: original,
                sequence: 7,
                events: vec![],
                event_times: vec![],
                payload: payload.map(str::to_owned),
                copy_delay: Duration::from_millis(75),
                copy_due: None,
                keys_until: Duration::ZERO,
                focus_until: None,
                cancel_at: None,
                cancel_on_backup: false,
                backup_fails: false,
                injection_fails: false,
                read_fails: false,
                restore_fails: false,
            }),
        }
    }

    fn read(&self) -> Result<String, SelectionError> {
        self.read_with_delays(WRITE_DELAY, RESTORE_DELAY)
    }

    fn read_with_delays(
        &self,
        write_delay: Duration,
        restore_delay: Duration,
    ) -> Result<String, SelectionError> {
        read_with(self, &self.cancel, write_delay, restore_delay)
    }

    fn assert_restored(&self) {
        let state = self.state.borrow();
        assert_eq!(state.clipboard, self.original);
        assert_eq!(state.events.last(), Some(&Event::Restore));
        assert_eq!(
            state
                .events
                .iter()
                .filter(|&&e| e == Event::Restore)
                .count(),
            1
        );
    }

    fn assert_untouched(&self) {
        let state = self.state.borrow();
        assert_eq!(state.clipboard, self.original);
        assert!(!state.events.contains(&Event::Copy));
        assert!(!state.events.contains(&Event::Restore));
    }
}

impl CopyBackend for FakeCopy {
    type Snapshot = Clipboard;

    fn target_is_current(&self) -> Result<bool, String> {
        let state = self.state.borrow();
        Ok(state.focus_until.is_none_or(|end| state.elapsed < end))
    }

    fn keys_pressed(&self) -> bool {
        let state = self.state.borrow();
        state.elapsed < state.keys_until
    }

    fn backup_and_clear(&self) -> Result<(Clipboard, u32), String> {
        let mut state = self.state.borrow_mut();
        state.record(Event::Backup);
        if state.backup_fails {
            return Err("backup failed".into());
        }
        let snapshot = state.clipboard.clone();
        state.clipboard = Clipboard {
            text: None,
            custom_data: vec![],
        };
        state.sequence += 1;
        if state.cancel_on_backup {
            self.cancel.cancel();
        }
        Ok((snapshot, state.sequence))
    }

    fn send_copy(&self) -> Result<(), String> {
        let mut state = self.state.borrow_mut();
        state.record(Event::Copy);
        if state.elapsed < state.keys_until {
            return Err("Copy injected before shortcut release".into());
        }
        if state.injection_fails {
            return Err("injection failed".into());
        }
        if state.payload.is_some() {
            state.copy_due = Some(state.elapsed + state.copy_delay);
        }
        Ok(())
    }

    fn read_changed(&self, sequence: u32) -> Result<Option<String>, String> {
        let state = self.state.borrow();
        if sequence != state.sequence && state.read_fails {
            return Err("read failed".into());
        }
        Ok((sequence != state.sequence)
            .then(|| state.clipboard.text.clone())
            .flatten())
    }

    fn restore(&self, snapshot: &Clipboard) -> Result<(), String> {
        let mut state = self.state.borrow_mut();
        state.record(Event::Restore);
        if state.restore_fails {
            return Err("restore failed".into());
        }
        state.clipboard = snapshot.clone();
        state.sequence += 1;
        Ok(())
    }

    fn now(&self) -> Instant {
        self.start + self.state.borrow().elapsed
    }

    fn sleep(&self, duration: Duration) {
        let mut state = self.state.borrow_mut();
        state.elapsed += duration;
        if state.cancel_at.is_some_and(|at| state.elapsed >= at) {
            self.cancel.cancel();
        }
        if state.copy_due.is_some_and(|at| state.elapsed >= at) {
            state.copy_due = None;
            state.clipboard.text = state.payload.clone();
            state.sequence += 1;
            state.record(Event::Copied);
        }
    }
}

#[test]
fn success_restores_original_text_and_custom_formats_before_returning() {
    let backend = FakeCopy::new(Some("selected text"));
    assert_eq!(backend.read().unwrap(), "selected text");
    backend.assert_restored();
    assert_eq!(
        backend.state.borrow().events,
        [Event::Backup, Event::Copy, Event::Copied, Event::Restore]
    );
}

#[test]
fn copying_the_same_text_as_the_original_clipboard_is_fresh_input() {
    let backend = FakeCopy::new(Some("original clipboard"));
    assert_eq!(backend.read().unwrap(), "original clipboard");
    backend.assert_restored();
    assert!(backend.state.borrow().events.contains(&Event::Copied));
}

#[test]
fn failed_copy_does_not_reuse_stale_clipboard_text() {
    let backend = FakeCopy::new(None);
    let error = backend.read().unwrap_err();
    assert!(matches!(error, SelectionError::Read(_)));
    assert!(error.to_string().contains("timed out"));
    assert!(backend.state.borrow().elapsed >= Duration::from_secs(3));
    backend.assert_restored();
}

#[test]
fn invalid_text_and_copy_errors_restore_the_clipboard() {
    for failure in ["empty", "oversized", "read", "injection"] {
        let backend = FakeCopy::new(Some("selection"));
        {
            let mut state = backend.state.borrow_mut();
            match failure {
                "empty" => state.payload = Some(" \r\n\t".into()),
                "oversized" => state.payload = Some("é".repeat(500_001)),
                "read" => {
                    state.read_fails = true;
                    state.copy_delay = Duration::from_millis(400);
                }
                "injection" => state.injection_fails = true,
                _ => unreachable!(),
            }
        }
        let error = backend.read().expect_err(failure);
        assert!(matches!(error, SelectionError::Read(_)), "{failure}");
        let expected = match failure {
            "empty" => "No text copied",
            "oversized" => "1,000,000 UTF-8 bytes",
            "read" => "read failed",
            "injection" => "injection failed",
            _ => unreachable!(),
        };
        assert!(error.to_string().contains(expected), "{failure}: {error}");
        backend.assert_restored();
    }
}

#[test]
fn backup_failure_prevents_copy() {
    let backend = FakeCopy::new(Some("selection"));
    backend.state.borrow_mut().backup_fails = true;
    assert!(
        backend
            .read()
            .unwrap_err()
            .to_string()
            .contains("backup failed")
    );
    backend.assert_untouched();
}

#[test]
fn held_shortcut_keys_do_not_touch_the_clipboard() {
    let backend = FakeCopy::new(Some("selection"));
    backend.state.borrow_mut().keys_until = Duration::from_secs(10);
    assert!(
        backend
            .read()
            .unwrap_err()
            .to_string()
            .contains("Release shortcut keys")
    );
    backend.assert_untouched();
    assert!(backend.state.borrow().events.is_empty());
}

#[test]
fn copy_waits_for_shortcut_keys_to_be_released() {
    let backend = FakeCopy::new(Some("selection"));
    backend.state.borrow_mut().keys_until = Duration::from_millis(300);
    assert_eq!(backend.read().unwrap(), "selection");
    assert!(backend.state.borrow().elapsed >= Duration::from_millis(300));
    backend.assert_restored();
}

#[test]
fn cancellation_before_backup_does_not_touch_the_clipboard() {
    let backend = FakeCopy::new(Some("selection"));
    backend.cancel.cancel();
    assert!(matches!(backend.read(), Err(SelectionError::Canceled)));
    backend.assert_untouched();
    assert!(backend.state.borrow().events.is_empty());
}

#[test]
fn cancellation_after_backup_restores_without_sending_copy() {
    let backend = FakeCopy::new(Some("selection"));
    backend.state.borrow_mut().cancel_on_backup = true;
    assert!(matches!(backend.read(), Err(SelectionError::Canceled)));
    backend.assert_restored();
    assert!(!backend.state.borrow().events.contains(&Event::Copy));
}

#[test]
fn cancellation_after_copy_waits_for_copy_completion_and_restores() {
    let backend = FakeCopy::new(Some("selection"));
    {
        let mut state = backend.state.borrow_mut();
        state.copy_delay = Duration::from_millis(400);
        state.cancel_at = Some(WRITE_DELAY + Duration::from_millis(50));
    }
    assert!(matches!(backend.read(), Err(SelectionError::Canceled)));
    backend.assert_restored();
    assert_eq!(
        backend.state.borrow().events,
        [Event::Backup, Event::Copy, Event::Copied, Event::Restore]
    );
}

#[test]
fn focus_changes_during_copy_or_settle_discard_text_and_restore() {
    for after_copy in [Duration::from_millis(50), Duration::from_millis(100)] {
        let backend = FakeCopy::new(Some("selection"));
        backend.state.borrow_mut().focus_until = Some(WRITE_DELAY + after_copy);
        assert!(
            backend
                .read()
                .unwrap_err()
                .to_string()
                .contains("Focus changed")
        );
        backend.assert_restored();
        assert!(backend.state.borrow().events.contains(&Event::Copied));
    }
}

#[test]
fn restore_failure_is_reported_even_when_reading_was_canceled() {
    for canceled in [false, true] {
        let backend = FakeCopy::new(Some("selection"));
        {
            let mut state = backend.state.borrow_mut();
            state.restore_fails = true;
            if canceled {
                state.cancel_at = Some(WRITE_DELAY + Duration::from_millis(25));
            }
        }
        let error = backend.read().unwrap_err();
        assert!(matches!(error, SelectionError::Restore(_)));
        assert!(error.to_string().contains("restore failed"));
        if canceled {
            assert!(error.to_string().contains("Request canceled"));
        }
        assert_eq!(backend.state.borrow().events.last(), Some(&Event::Restore));
    }
}

#[test]
fn configured_delays_apply_between_clear_copy_and_restore_including_zero() {
    for (write_ms, restore_ms) in [(0, 0), (80, 120), (237, 411)] {
        let backend = FakeCopy::new(Some("selection"));
        // Waiting for the shortcut release must not consume the write delay.
        backend.state.borrow_mut().keys_until = Duration::from_millis(300);
        let write_delay = Duration::from_millis(write_ms);
        let restore_delay = Duration::from_millis(restore_ms);
        assert_eq!(
            backend
                .read_with_delays(write_delay, restore_delay)
                .unwrap(),
            "selection"
        );
        backend.assert_restored();
        let state = backend.state.borrow();
        assert_eq!(
            state.event_time(Event::Copy) - state.event_time(Event::Backup),
            write_delay
        );
        assert_eq!(
            state.event_time(Event::Restore) - state.event_time(Event::Copied),
            restore_delay
        );
    }
}

#[test]
fn cancellation_during_write_delay_restores_without_sending_copy() {
    let backend = FakeCopy::new(Some("selection"));
    backend.state.borrow_mut().cancel_at = Some(Duration::from_millis(35));
    let write_delay = Duration::from_millis(500);
    assert!(matches!(
        backend.read_with_delays(write_delay, RESTORE_DELAY),
        Err(SelectionError::Canceled)
    ));
    backend.assert_restored();
    let state = backend.state.borrow();
    assert_eq!(state.events, [Event::Backup, Event::Restore]);
    assert!(state.event_time(Event::Restore) < write_delay);
}

#[test]
fn focus_change_during_write_delay_restores_without_sending_copy() {
    let backend = FakeCopy::new(Some("selection"));
    backend.state.borrow_mut().focus_until = Some(Duration::from_millis(35));
    let write_delay = Duration::from_millis(500);
    assert!(
        backend
            .read_with_delays(write_delay, RESTORE_DELAY)
            .unwrap_err()
            .to_string()
            .contains("Focus changed")
    );
    backend.assert_restored();
    let state = backend.state.borrow();
    assert_eq!(state.events, [Event::Backup, Event::Restore]);
    assert!(state.event_time(Event::Restore) < write_delay);
}

#[test]
fn cancellation_during_restore_delay_preserves_the_configured_wait() {
    let backend = FakeCopy::new(Some("selection"));
    let write_delay = Duration::from_millis(37);
    let restore_delay = Duration::from_millis(443);
    backend.state.borrow_mut().cancel_at = Some(write_delay + Duration::from_millis(85));
    assert!(matches!(
        backend.read_with_delays(write_delay, restore_delay),
        Err(SelectionError::Canceled)
    ));
    backend.assert_restored();
    let state = backend.state.borrow();
    assert_eq!(
        state.event_time(Event::Restore) - state.event_time(Event::Copied),
        restore_delay
    );
}
