//! Own posted pointer data independently of the target's pointer width.
use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PointerEvent {
    pub generation: u32,
    pub message: u32,
    pub x: i32,
    pub y: i32,
}

#[derive(Default)]
pub(super) struct PointerQueue {
    events: VecDeque<PointerEvent>,
}

impl PointerQueue {
    /// Keep exactly one event for every successfully posted window message.
    pub fn post(&mut self, event: PointerEvent, post_message: impl FnOnce() -> bool) -> bool {
        self.events.push_back(event);
        if post_message() {
            true
        } else {
            self.events.pop_back();
            false
        }
    }

    /// Consume one event even when it belongs to an earlier menu opening.
    pub fn take(&mut self, generation: u32) -> Option<PointerEvent> {
        self.events
            .pop_front()
            .filter(|event| event.generation == generation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_full_coordinates_and_order_across_menu_generations() {
        let mut queue = PointerQueue::default();
        let old = PointerEvent {
            generation: 1,
            message: 1,
            x: -100_000,
            y: 150_000,
        };
        let current = PointerEvent {
            generation: 2,
            message: 2,
            x: i32::MAX,
            y: i32::MIN,
        };
        assert!(queue.post(old, || true));
        assert!(queue.post(current, || true));
        assert_eq!(queue.take(2), None);
        assert_eq!(queue.take(2), Some(current));
        assert_eq!(queue.take(2), None);
    }

    #[test]
    fn failed_post_discards_only_its_own_event() {
        let mut queue = PointerQueue::default();
        let event = PointerEvent {
            generation: 4,
            message: 9,
            x: 17,
            y: -21,
        };
        assert!(queue.post(event, || true));
        assert!(!queue.post(
            PointerEvent {
                message: 10,
                ..event
            },
            || false
        ));
        assert_eq!(queue.take(4), Some(event));
        assert_eq!(queue.take(4), None);
    }
}
