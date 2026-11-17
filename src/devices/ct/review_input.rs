//! Offline fesReview candidate input reduction, not native input authority.
//!
//! One instance represents one already-separated input byte stream. It must not
//! consume diagnostic report dumps, handshake/reply bytes, or reconnects.
//! Neither constructing this reducer nor receiving candidate actions qualifies
//! hardware, verifies identity/firmware, or permits action delivery.

use serde::{Deserialize, Serialize};

use super::protocol::{ButtonState, FrameDecoder, InputEvent, decode_input};

pub const MAX_CHUNK_BYTES: usize = 4096;
pub const MAX_BATCH_ACTIONS: usize = 128;
const MAX_BATCH_FRAMES: usize = 128;
const BUTTON_COOLDOWN_MS: u64 = 75;

/// Proposed action-only wire vocabulary. These values are offline candidates.
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

/// No readiness, routing, command, argument, or target fields are admitted.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateAction {
    pub action: Action,
}

impl CandidateAction {
    /// A fixed, bounded proposed NDJSON record; never written by this module.
    pub const fn json_line(self) -> &'static str {
        match self.action {
            Action::TogglePlay => "{\"action\":\"toggle_play\"}\n",
            Action::SeekStart => "{\"action\":\"seek_start\"}\n",
            Action::SeekEnd => "{\"action\":\"seek_end\"}\n",
            Action::BackOneSecond => "{\"action\":\"back_one_second\"}\n",
            Action::ForwardOneSecond => "{\"action\":\"forward_one_second\"}\n",
            Action::Screenshot => "{\"action\":\"screenshot\"}\n",
            Action::ToggleDictation => "{\"action\":\"toggle_dictation\"}\n",
            Action::PreviousFrame => "{\"action\":\"previous_frame\"}\n",
            Action::NextFrame => "{\"action\":\"next_frame\"}\n",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputFailure {
    Ended,
    ChunkLimit,
    FrameLimit,
    ActionLimit,
    Framing,
    InvalidInput,
    ClockRegression,
}

#[derive(Clone, Copy, Default)]
enum Button {
    #[default]
    Unknown,
    Released,
    Pressed,
    Suppressed,
}

const ROUND_ACTIONS: [Action; 7] = [
    Action::TogglePlay,
    Action::SeekStart,
    Action::SeekEnd,
    Action::BackOneSecond,
    Action::ForwardOneSecond,
    Action::Screenshot,
    Action::ToggleDictation,
];

/// Bounded, input-only candidate decoder with terminal stream boundaries.
///
/// Buttons require an observed Up before a Down/Up pair. Repeated edges do not
/// fire, and new presses within 75ms of emission are suppressed until release.
/// This synthetic policy is not a measured physical debounce qualification.
/// Wheel ID zero maps each signed delta unit to one candidate frame action;
/// physical direction and the meaning of a delta unit still need acceptance.
#[derive(Default)]
pub struct CandidateStream {
    decoder: FrameDecoder,
    buttons: [Button; 7],
    emitted_at: [Option<u64>; 7],
    last_ms: Option<u64>,
    ended: bool,
}

impl CandidateStream {
    /// Consume at most one bounded raw-input chunk using monotonic milliseconds.
    ///
    /// The batch is atomic: any fault discards all actions from this call,
    /// clears held state, and permanently ends this instance. Callers must also
    /// discard any outstanding batches on loss of transport/foreground.
    pub fn push(
        &mut self,
        bytes: &[u8],
        now_ms: u64,
    ) -> Result<Vec<CandidateAction>, InputFailure> {
        if self.ended {
            return Err(InputFailure::Ended);
        }
        let result = self.push_inner(bytes, now_ms);
        if result.is_err() {
            self.end();
        }
        result
    }

    fn push_inner(
        &mut self,
        mut bytes: &[u8],
        now_ms: u64,
    ) -> Result<Vec<CandidateAction>, InputFailure> {
        if bytes.len() > MAX_CHUNK_BYTES {
            return Err(InputFailure::ChunkLimit);
        }
        if self.last_ms.is_some_and(|last| now_ms < last) {
            return Err(InputFailure::ClockRegression);
        }
        self.last_ms = Some(now_ms);
        let mut actions = Vec::new();
        let mut frames = 0;
        while !bytes.is_empty() {
            let step = self
                .decoder
                .decode(bytes)
                .map_err(|_| InputFailure::Framing)?;
            bytes = &bytes[step.consumed..];
            if let Some(payload) = step.frame {
                frames += 1;
                if frames > MAX_BATCH_FRAMES {
                    return Err(InputFailure::FrameLimit);
                }
                let event = decode_input(&payload).map_err(|_| InputFailure::InvalidInput)?;
                match event {
                    InputEvent::Button { control, state, .. }
                        if (0x07..=0x0d).contains(&control.value()) =>
                    {
                        let index = usize::from(control.value() - 0x07);
                        match (self.buttons[index], state) {
                            (Button::Released, ButtonState::Down) => {
                                self.buttons[index] = if self.emitted_at[index]
                                    .is_some_and(|last| now_ms - last < BUTTON_COOLDOWN_MS)
                                {
                                    Button::Suppressed
                                } else {
                                    Button::Pressed
                                };
                            }
                            (Button::Pressed, ButtonState::Up) => {
                                Self::append(&mut actions, ROUND_ACTIONS[index], 1)?;
                                self.emitted_at[index] = Some(now_ms);
                                self.buttons[index] = Button::Released;
                            }
                            (_, ButtonState::Up) => self.buttons[index] = Button::Released,
                            _ => {}
                        }
                    }
                    InputEvent::Rotation { control, delta, .. } if control.value() == 0 => {
                        let action = if delta < 0 {
                            Action::PreviousFrame
                        } else {
                            Action::NextFrame
                        };
                        Self::append(&mut actions, action, usize::from(delta.unsigned_abs()))?;
                    }
                    _ => {}
                }
            }
        }
        Ok(actions)
    }

    fn append(
        actions: &mut Vec<CandidateAction>,
        action: Action,
        count: usize,
    ) -> Result<(), InputFailure> {
        if count > MAX_BATCH_ACTIONS - actions.len() {
            return Err(InputFailure::ActionLimit);
        }
        actions.extend(std::iter::repeat_n(CandidateAction { action }, count));
        Ok(())
    }

    /// Terminate on EOF, cancellation, transport failure, or foreground loss.
    /// A new stream always starts disarmed; this instance cannot be reset.
    pub fn end(&mut self) {
        self.ended = true;
        self.decoder.reset();
        self.buttons.fill(Button::Unknown);
        self.emitted_at.fill(None);
        self.last_ms = None;
    }

    /// EOF discards held presses and rejects a truncated trailing frame.
    pub fn eof(&mut self) -> Result<(), InputFailure> {
        if self.ended {
            return Err(InputFailure::Ended);
        }
        let result = self.decoder.eof().map_err(|_| InputFailure::Framing);
        self.end();
        result
    }
}

#[cfg(test)]
mod tests;
