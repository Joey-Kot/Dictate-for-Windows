//! The GUI owns its bounded session log. No HWND or UI work crosses into a
//! producer callback, and snapshots release the lock before composing text.
use std::collections::VecDeque;
use std::sync::Arc;

use dictate_core::debug_log::Record;

const MAX_LINES: usize = 2000;
const MAX_BYTES: usize = 1024 * 1024;

struct Line {
    text: Arc<str>,
    utf16: u64,
}

struct Buffer {
    lines: VecDeque<Line>,
    bytes: usize,
    start: u64,
    end: u64,
    revision: u64,
    max_lines: usize,
    max_bytes: usize,
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new(MAX_LINES, MAX_BYTES)
    }
}

impl Buffer {
    fn new(max_lines: usize, max_bytes: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            bytes: 0,
            start: 0,
            end: 0,
            revision: 1,
            max_lines,
            max_bytes,
        }
    }

    fn append(&mut self, record: Record) {
        let time: chrono::DateTime<chrono::Local> = record.timestamp.into();
        let prefix = format!(
            "[{}] [{}] ",
            time.format("%H:%M:%S%.3f"),
            record.category.label()
        );
        let message = record.message.replace('\0', "�");
        for line in message.lines().filter(|line| !line.trim().is_empty()) {
            let line = [
                "[upload] ",
                "[record] ",
                "[hotkey] ",
                "[hotkey-debug] ",
                "[ffmpeg] ",
            ]
            .iter()
            .find_map(|tag| line.strip_prefix(tag))
            .unwrap_or(line);
            let text = format!("{prefix}{}\r\n", line.replace('\r', ""));
            let utf16 = text.encode_utf16().count() as u64;
            self.bytes += text.len();
            self.end += utf16;
            self.lines.push_back(Line {
                text: text.into(),
                utf16,
            });
            while self.lines.len() > self.max_lines || self.bytes > self.max_bytes {
                let first = self.lines.pop_front().unwrap();
                self.start += first.utf16;
                self.bytes -= first.text.len();
            }
        }
        self.revision = self.revision.wrapping_add(1);
    }

    fn clear(&mut self) {
        self.lines.clear();
        self.bytes = 0;
        self.start = self.end;
        self.revision = self.revision.wrapping_add(1);
    }

    fn snapshot(&self, previous: u64) -> Option<Snapshot> {
        (previous != self.revision).then(|| Snapshot {
            lines: self.lines.iter().map(|line| line.text.clone()).collect(),
            revision: self.revision,
            start: self.start,
            end: self.end,
        })
    }
}

pub struct Snapshot {
    lines: Vec<Arc<str>>,
    pub revision: u64,
    pub start: u64,
    end: u64,
}

impl Snapshot {
    pub fn text(&self) -> String {
        self.lines.iter().map(|line| line.as_ref()).collect()
    }

    /// Native EDIT offsets count UTF-16 units, including the CRLF separators.
    pub fn rebase(&self, previous_start: u64, position: u32) -> i32 {
        previous_start
            .saturating_add(u64::from(position))
            .saturating_sub(self.start)
            .min(self.end - self.start)
            .min(i32::MAX as u64) as i32
    }
}

#[cfg(windows)]
fn buffer() -> &'static parking_lot::Mutex<Buffer> {
    static BUFFER: std::sync::OnceLock<parking_lot::Mutex<Buffer>> = std::sync::OnceLock::new();
    BUFFER.get_or_init(|| parking_lot::Mutex::new(Buffer::default()))
}

#[cfg(windows)]
pub fn install() -> dictate_core::debug_log::Subscription {
    dictate_core::debug_log::subscribe(|entry| buffer().lock().append(entry))
}

#[cfg(windows)]
pub fn snapshot(previous: u64) -> Option<Snapshot> {
    buffer().lock().snapshot(previous)
}

#[cfg(windows)]
pub fn clear() {
    buffer().lock().clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use dictate_core::debug_log::Category;

    fn record(message: &str) -> Record {
        Record {
            timestamp: std::time::UNIX_EPOCH,
            category: Category::Upload,
            message: message.into(),
        }
    }

    #[test]
    fn snapshots_survive_clear_and_receiving_new_records() {
        let mut buffer = Buffer::default();
        buffer.append(record("first\nsecond\0line"));
        let first = buffer.snapshot(0).unwrap();
        assert!(first.text().contains("[Upload] first\r\n"));
        assert!(first.text().contains("second�line\r\n"));
        assert!(buffer.snapshot(first.revision).is_none());
        buffer.clear();
        let cleared = buffer.snapshot(first.revision).unwrap();
        assert!(cleared.text().is_empty());
        assert_eq!(cleared.rebase(first.start, 20), 0);
        buffer.append(record("after clear"));
        assert!(
            buffer
                .snapshot(cleared.revision)
                .unwrap()
                .text()
                .contains("after clear")
        );
        assert!(first.text().contains("first"));
    }

    #[test]
    fn trimming_preserves_utf16_selection_offsets_in_retained_lines() {
        let mut buffer = Buffer::new(2, MAX_BYTES);
        buffer.append(record("旧🙂"));
        buffer.append(record("保留🙂文字"));
        let before = buffer.snapshot(0).unwrap();
        let text = before.text();
        let selected = text.find("保留").unwrap();
        let offset = text[..selected].encode_utf16().count() as u32;
        let removed = buffer.lines[0].utf16;
        buffer.append(record("latest"));
        let after = buffer.snapshot(before.revision).unwrap();
        assert!(!after.text().contains("旧"));
        assert_eq!(
            after.rebase(before.start, offset),
            (u64::from(offset) - removed) as i32
        );
        assert_eq!(after.rebase(before.start, 0), 0);
        assert_eq!(
            after.rebase(before.start, u32::MAX) as usize,
            after.text().encode_utf16().count()
        );
    }

    #[test]
    fn byte_and_line_limits_apply_even_to_multiline_bursts() {
        let mut buffer = Buffer::new(3, 140);
        buffer.append(record(&"🙂🙂\n".repeat(50)));
        assert!(buffer.lines.len() <= 3);
        assert!(buffer.bytes <= 140);
        assert_eq!(
            buffer.end - buffer.start,
            buffer.snapshot(0).unwrap().text().encode_utf16().count() as u64
        );
        buffer.append(record(&"x".repeat(1000)));
        assert!(buffer.lines.is_empty());
        assert_eq!(buffer.start, buffer.end);
    }
}
