//! Strict foreground fesReview action reduction for exact MX Creative reports.
//!
//! This module contains no native access. Its caller must first validate and
//! open the two exact HID collections inside the dedicated streaming worker.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::probe::Role;

pub const DIAL_COALESCE: Duration = Duration::from_millis(100);
pub const ARM_DRAIN: Duration = Duration::from_millis(50);
pub const READ_SLICE: Duration = Duration::from_millis(10);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    TogglePlay,
    SeekStart,
    SeekEnd,
    BackOneSecond,
    ForwardOneSecond,
    Screenshot,
    ToggleDictation,
    PreviousFrame,
    NextFrame,
}

impl Action {
    pub const fn json_line(self) -> &'static str {
        match self {
            Self::TogglePlay => "{\"action\":\"toggle_play\"}\n",
            Self::SeekStart => "{\"action\":\"seek_start\"}\n",
            Self::SeekEnd => "{\"action\":\"seek_end\"}\n",
            Self::BackOneSecond => "{\"action\":\"back_one_second\"}\n",
            Self::ForwardOneSecond => "{\"action\":\"forward_one_second\"}\n",
            Self::Screenshot => "{\"action\":\"screenshot\"}\n",
            Self::ToggleDictation => "{\"action\":\"toggle_dictation\"}\n",
            Self::PreviousFrame => "{\"action\":\"previous_frame\"}\n",
            Self::NextFrame => "{\"action\":\"next_frame\"}\n",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Direction {
    Negative,
    Positive,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Reducer {
    keypad_value: u8,
    dial: Option<(Direction, Duration)>,
}

impl Reducer {
    pub fn accept(&mut self, role: Role, report: &[u8], now: Duration) -> Option<Action> {
        match role {
            Role::Keypad => self.keypad(report),
            Role::Dialpad => self.dialpad(report, now),
        }
    }

    pub fn arm_baseline(&mut self, role: Role, report: &[u8]) {
        if role == Role::Keypad && report.len() == 32 && report[0] == 19 {
            self.keypad_value = report[6];
        }
    }

    fn keypad(&mut self, report: &[u8]) -> Option<Action> {
        if report.len() != 32 || report[0] != 19 {
            return None;
        }
        let value = report[6];
        let changed = value != self.keypad_value;
        self.keypad_value = value;
        if !changed || value == 0 || value >= 8 {
            return None;
        }
        Some(match value {
            1 => Action::TogglePlay,
            2 => Action::SeekStart,
            3 => Action::SeekEnd,
            4 => Action::BackOneSecond,
            5 => Action::ForwardOneSecond,
            6 => Action::Screenshot,
            7 => Action::ToggleDictation,
            _ => unreachable!("value range checked"),
        })
    }

    fn dialpad(&mut self, report: &[u8], now: Duration) -> Option<Action> {
        if report.len() != 8 || report[0] != 2 {
            return None;
        }
        let ac_pan = report[7] as i8;
        let direction = match ac_pan {
            ..=-1 => Direction::Negative,
            0 => return None,
            1.. => Direction::Positive,
        };
        if let Some((previous, emitted_at)) = self.dial
            && previous == direction
            && now.saturating_sub(emitted_at) < DIAL_COALESCE
        {
            return None;
        }
        self.dial = Some((direction, now));
        Some(match direction {
            Direction::Negative => Action::PreviousFrame,
            Direction::Positive => Action::NextFrame,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keypad(value: u8) -> Vec<u8> {
        let mut report = vec![0; 32];
        report[0] = 19;
        report[6] = value;
        report
    }

    fn dial(ac_pan: i8) -> Vec<u8> {
        vec![2, 0, 0, 0, 0, 0, 0, ac_pan as u8]
    }

    #[test]
    fn seven_keypad_buttons_map_in_order_and_release_or_unknown_is_silent() {
        let expected = [
            Action::TogglePlay,
            Action::SeekStart,
            Action::SeekEnd,
            Action::BackOneSecond,
            Action::ForwardOneSecond,
            Action::Screenshot,
            Action::ToggleDictation,
        ];
        for (index, expected) in (1..=7).zip(expected) {
            let mut reducer = Reducer::default();
            assert_eq!(
                reducer.accept(Role::Keypad, &keypad(index), Duration::ZERO),
                Some(expected)
            );
            assert_eq!(
                reducer.accept(Role::Keypad, &keypad(index), Duration::ZERO),
                None
            );
            assert_eq!(
                reducer.accept(Role::Keypad, &keypad(0), Duration::ZERO),
                None
            );
        }
        for value in [8, 9] {
            assert_eq!(
                Reducer::default().accept(Role::Keypad, &keypad(value), Duration::ZERO),
                None
            );
        }
    }

    #[test]
    fn dial_uses_only_signed_ac_pan_byte_with_bounded_coalescing() {
        let mut reducer = Reducer::default();
        let mut positive = dial(1);
        positive[1] = 0xff;
        positive[2] = 0xff;
        positive[6] = 0xff;
        assert_eq!(
            reducer.accept(Role::Dialpad, &positive, Duration::from_millis(10)),
            Some(Action::NextFrame)
        );
        assert_eq!(
            reducer.accept(Role::Dialpad, &dial(13), Duration::from_millis(109)),
            None
        );
        assert_eq!(
            reducer.accept(Role::Dialpad, &dial(-1), Duration::from_millis(110)),
            Some(Action::PreviousFrame)
        );
        assert_eq!(
            reducer.accept(Role::Dialpad, &dial(-5), Duration::from_millis(120)),
            None
        );
        assert_eq!(
            reducer.accept(Role::Dialpad, &dial(1), Duration::from_millis(121)),
            Some(Action::NextFrame)
        );
        assert_eq!(
            reducer.accept(Role::Dialpad, &dial(1), Duration::from_millis(221)),
            Some(Action::NextFrame)
        );
    }

    #[test]
    fn reducer_rejects_other_reports_and_zero_ac_pan() {
        let mut reducer = Reducer::default();
        let mut nominal_wheel_only = dial(0);
        nominal_wheel_only[6] = 0xff;
        assert_eq!(
            reducer.accept(Role::Dialpad, &nominal_wheel_only, Duration::ZERO),
            None
        );
        assert_eq!(
            reducer.accept(Role::Dialpad, &[2, 0, 0, 0, 0, 0, 0, 0], Duration::ZERO),
            None
        );
        assert_eq!(
            reducer.accept(Role::Dialpad, &[17, 0, 0, 0, 0, 0, 0, 1], Duration::ZERO),
            None
        );
        assert_eq!(reducer.accept(Role::Keypad, &[4, 1], Duration::ZERO), None);
    }

    #[test]
    fn arm_baseline_suppresses_a_held_press_until_release_and_new_press() {
        let mut reducer = Reducer::default();
        reducer.arm_baseline(Role::Keypad, &keypad(3));
        assert_eq!(
            reducer.accept(Role::Keypad, &keypad(3), Duration::ZERO),
            None
        );
        assert_eq!(
            reducer.accept(Role::Keypad, &keypad(0), Duration::from_millis(1)),
            None
        );
        assert_eq!(
            reducer.accept(Role::Keypad, &keypad(3), Duration::from_millis(2)),
            Some(Action::SeekEnd)
        );
    }

    #[test]
    fn arm_baseline_ignores_all_non_keypad_state_and_other_reports() {
        let mut reducer = Reducer::default();
        reducer.arm_baseline(Role::Keypad, &[4, 1]);
        reducer.arm_baseline(Role::Dialpad, &dial(1));
        assert_eq!(
            reducer.accept(Role::Keypad, &keypad(1), Duration::ZERO),
            Some(Action::TogglePlay)
        );
        assert_eq!(
            reducer.accept(Role::Dialpad, &dial(1), Duration::ZERO),
            Some(Action::NextFrame)
        );
    }
}
