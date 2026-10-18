//! Decisions for physical keys handled by the low-level keyboard hook.
use std::collections::HashSet;

use super::{MOD_ALT, MOD_CTRL};

/// Candidates share one virtual key. Additional held modifiers belong to a
/// different chord and must never trigger a less specific binding.
pub(super) fn matching_action(candidates: &[(i32, u32)], pressed_modifiers: u32) -> Option<i32> {
    candidates
        .iter()
        .find_map(|&(id, modifiers)| (modifiers == pressed_modifiers).then_some(id))
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum KeyDown {
    Pass,
    Suppress,
    Trigger { id: i32, mask_alt: bool },
}

#[derive(Default)]
pub(super) struct HookKeys {
    swallowed: HashSet<u32>,
}

impl HookKeys {
    pub(super) fn clear(&mut self) {
        self.swallowed.clear();
    }

    pub(super) fn key_down(
        &mut self,
        virtual_key: u32,
        matched_action: Option<i32>,
        pressed_modifiers: u32,
    ) -> KeyDown {
        // Auto-repeat stays swallowed even after a modifier is released.
        if self.swallowed.contains(&virtual_key) {
            return KeyDown::Suppress;
        }
        let Some(id) = matched_action else {
            return KeyDown::Pass;
        };
        self.swallowed.insert(virtual_key);
        // Without the swallowed main key, the foreground app would see a bare
        // Alt press/release and move focus to its menu bar. Ctrl+Alt (including
        // AltGr) does not activate the menu bar and needs no injected mask.
        KeyDown::Trigger {
            id,
            mask_alt: pressed_modifiers & (MOD_ALT | MOD_CTRL) == MOD_ALT,
        }
    }

    pub(super) fn key_up(&mut self, virtual_key: u32) -> bool {
        self.swallowed.remove(&virtual_key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotkey::{MOD_SHIFT, MOD_WIN, REWRITE_MENU_ACTION, REWRITE_PROMPT_ACTION_BASE};

    const A: u32 = 0x41;
    const ALT: u32 = 0xA4;

    #[test]
    fn matching_requires_the_complete_chord_for_audio_menu_and_prompt_actions() {
        let mut candidates = vec![
            (1, 0),
            (2, MOD_CTRL),
            (3, MOD_SHIFT),
            (REWRITE_MENU_ACTION, MOD_ALT),
            (REWRITE_PROMPT_ACTION_BASE, MOD_WIN),
        ];
        for &(id, modifiers) in &candidates {
            assert_eq!(matching_action(&candidates, modifiers), Some(id));
        }
        assert_eq!(matching_action(&candidates, MOD_CTRL | MOD_SHIFT), None);
        candidates.push((REWRITE_PROMPT_ACTION_BASE + 1, MOD_CTRL | MOD_SHIFT));
        assert_eq!(
            matching_action(&candidates, MOD_CTRL | MOD_SHIFT),
            Some(REWRITE_PROMPT_ACTION_BASE + 1)
        );
        candidates.reverse();
        assert_eq!(matching_action(&candidates, MOD_CTRL), Some(2));
        assert_eq!(matching_action(&candidates, 0), Some(1));
        assert_eq!(
            matching_action(&candidates, MOD_CTRL | MOD_SHIFT | MOD_ALT),
            None
        );
    }

    #[test]
    fn all_sixteen_modifier_combinations_are_isolated() {
        for bound in 0..16 {
            let id = REWRITE_PROMPT_ACTION_BASE + bound as i32;
            let candidates = [(id, bound)];
            for pressed in 0..16 {
                assert_eq!(
                    matching_action(&candidates, pressed),
                    (bound == pressed).then_some(id),
                    "bound modifiers={bound:#x}, pressed modifiers={pressed:#x}"
                );
            }
        }
    }

    #[test]
    fn modifier_changes_cannot_retrigger_a_swallowed_main_key() {
        let candidates = [(1, MOD_CTRL), (REWRITE_MENU_ACTION, MOD_CTRL | MOD_SHIFT)];
        let mut keys = HookKeys::default();
        assert_eq!(
            keys.key_down(A, matching_action(&candidates, MOD_CTRL), MOD_CTRL),
            KeyDown::Trigger {
                id: 1,
                mask_alt: false
            }
        );
        for modifiers in [MOD_CTRL | MOD_SHIFT, 0, MOD_ALT, MOD_CTRL] {
            assert_eq!(
                keys.key_down(A, matching_action(&candidates, modifiers), modifiers),
                KeyDown::Suppress
            );
        }
        assert!(keys.key_up(A));
        assert_eq!(
            keys.key_down(
                A,
                matching_action(&candidates, MOD_CTRL | MOD_SHIFT),
                MOD_CTRL | MOD_SHIFT,
            ),
            KeyDown::Trigger {
                id: REWRITE_MENU_ACTION,
                mask_alt: false
            }
        );
    }

    #[test]
    fn alt_menu_chord_masks_once_and_keeps_modifier_releases() {
        let mut keys = HookKeys::default();
        assert_eq!(keys.key_down(ALT, None, 0), KeyDown::Pass);
        assert_eq!(
            keys.key_down(A, Some(REWRITE_MENU_ACTION), MOD_ALT),
            KeyDown::Trigger {
                id: REWRITE_MENU_ACTION,
                mask_alt: true
            }
        );
        assert_eq!(
            keys.key_down(A, Some(REWRITE_MENU_ACTION), MOD_ALT),
            KeyDown::Suppress
        );
        assert!(!keys.key_up(ALT));
        assert_eq!(keys.key_down(A, None, 0), KeyDown::Suppress);
        assert!(keys.key_up(A));
        assert!(!keys.key_up(A));
        assert_eq!(keys.key_down(A, None, 0), KeyDown::Pass);
    }

    #[test]
    fn releasing_main_key_first_still_passes_alt_release() {
        let mut keys = HookKeys::default();
        keys.key_down(A, Some(REWRITE_MENU_ACTION), MOD_ALT);
        assert!(keys.key_up(A));
        assert!(!keys.key_up(ALT));
        assert_eq!(
            keys.key_down(A, Some(REWRITE_MENU_ACTION), MOD_ALT),
            KeyDown::Trigger {
                id: REWRITE_MENU_ACTION,
                mask_alt: true
            }
        );
    }

    #[test]
    fn unmatched_alt_chords_do_not_get_masked_or_swallowed() {
        let mut keys = HookKeys::default();
        assert_eq!(keys.key_down(A, None, MOD_ALT), KeyDown::Pass);
        assert!(!keys.key_up(A));
        assert!(!keys.key_up(ALT));
    }

    #[test]
    fn ctrl_alt_and_non_alt_chords_do_not_inject_keys() {
        for modifiers in [
            0,
            MOD_CTRL,
            MOD_SHIFT,
            MOD_ALT | MOD_CTRL,
            MOD_ALT | MOD_CTRL | MOD_SHIFT,
        ] {
            let mut keys = HookKeys::default();
            assert_eq!(
                keys.key_down(A, Some(REWRITE_MENU_ACTION), modifiers),
                KeyDown::Trigger {
                    id: REWRITE_MENU_ACTION,
                    mask_alt: false
                }
            );
        }
    }

    #[test]
    fn masking_uses_actual_alt_state_for_all_consumed_actions() {
        // Every consumed Alt chord needs the menu mask, independently of
        // whether it belongs to an audio action, the menu or a prompt.
        for id in [1, 2, 3, REWRITE_MENU_ACTION, 1000] {
            let mut keys = HookKeys::default();
            assert_eq!(
                keys.key_down(A, Some(id), MOD_ALT | MOD_SHIFT),
                KeyDown::Trigger { id, mask_alt: true }
            );
        }
    }

    #[test]
    fn capture_reset_preserves_recorders_existing_passthrough() {
        let mut keys = HookKeys::default();
        keys.key_down(A, Some(REWRITE_MENU_ACTION), MOD_ALT);
        keys.clear();
        assert!(!keys.key_up(A));
        assert_eq!(keys.key_down(A, None, MOD_ALT), KeyDown::Pass);
    }
}
