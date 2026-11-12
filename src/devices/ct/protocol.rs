//! Incremental CT receive framing and exact input-message decoding.

use super::controls::RawControlId;

const MARKER: u8 = 0x82;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameError {
    InvalidMaximum(usize),
    UnexpectedMarker(u8),
    InvalidLength { length: u8, maximum: u8 },
    TruncatedFrame,
    Faulted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodeStep {
    pub consumed: usize,
    pub frame: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Marker,
    Length,
    Payload,
}

/// One-frame-at-a-time decoder. A failed chunk and its suffix must be discarded.
pub struct FrameDecoder {
    maximum: u8,
    phase: Phase,
    expected: usize,
    filled: usize,
    payload: [u8; 255],
    faulted: bool,
}

impl std::fmt::Debug for FrameDecoder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FrameDecoder")
            .field("maximum", &self.maximum)
            .field("phase", &self.phase)
            .field("expected", &self.expected)
            .field("filled", &self.filled)
            .field("faulted", &self.faulted)
            .finish_non_exhaustive()
    }
}

impl FrameDecoder {
    pub fn new(maximum: usize) -> Result<Self, FrameError> {
        if !(3..=255).contains(&maximum) {
            return Err(FrameError::InvalidMaximum(maximum));
        }
        Ok(Self {
            maximum: maximum as u8,
            phase: Phase::Marker,
            expected: 0,
            filled: 0,
            payload: [0; 255],
            faulted: false,
        })
    }

    pub fn is_faulted(&self) -> bool {
        self.faulted
    }

    fn fault(&mut self) {
        self.payload.fill(0);
        self.expected = 0;
        self.filled = 0;
        self.faulted = true;
    }

    pub fn decode(&mut self, input: &[u8]) -> Result<DecodeStep, FrameError> {
        if self.faulted {
            return Err(FrameError::Faulted);
        }
        let mut consumed = 0;
        while consumed < input.len() {
            match self.phase {
                Phase::Marker => {
                    let marker = input[consumed];
                    consumed += 1;
                    if marker != MARKER {
                        self.fault();
                        return Err(FrameError::UnexpectedMarker(marker));
                    }
                    self.phase = Phase::Length;
                }
                Phase::Length => {
                    let length = input[consumed];
                    consumed += 1;
                    if length < 3 || length > self.maximum {
                        self.fault();
                        return Err(FrameError::InvalidLength {
                            length,
                            maximum: self.maximum,
                        });
                    }
                    self.expected = usize::from(length);
                    self.filled = 0;
                    self.phase = Phase::Payload;
                }
                Phase::Payload => {
                    let count = (self.expected - self.filled).min(input.len() - consumed);
                    self.payload[self.filled..self.filled + count]
                        .copy_from_slice(&input[consumed..consumed + count]);
                    self.filled += count;
                    consumed += count;
                    if self.filled == self.expected {
                        let frame = self.payload[..self.expected].to_vec();
                        self.payload[..self.expected].fill(0);
                        self.phase = Phase::Marker;
                        self.expected = 0;
                        self.filled = 0;
                        return Ok(DecodeStep {
                            consumed,
                            frame: Some(frame),
                        });
                    }
                }
            }
        }
        Ok(DecodeStep {
            consumed,
            frame: None,
        })
    }

    /// Signals the end of the current byte stream, not a transport reconnect.
    pub fn eof(&mut self) -> Result<(), FrameError> {
        if self.faulted {
            return Err(FrameError::Faulted);
        }
        if self.phase != Phase::Marker {
            self.fault();
            return Err(FrameError::TruncatedFrame);
        }
        Ok(())
    }

    /// Discards the old prefix and fault. The caller must supply a new generation.
    pub fn reset(&mut self) {
        self.phase = Phase::Marker;
        self.expected = 0;
        self.filled = 0;
        self.faulted = false;
        self.payload.fill(0);
    }
}

impl Default for FrameDecoder {
    fn default() -> Self {
        Self::new(255).expect("the default CT frame maximum is valid")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonState {
    Down,
    Up,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchFamily {
    Standard,
    Ct,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchPhase {
    Contact,
    End,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Touch {
    pub transaction: u8,
    pub family: TouchFamily,
    pub phase: TouchPhase,
    pub unknown: u8,
    pub x: u16,
    pub y: u16,
    pub contact_id: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputEvent {
    Tick {
        transaction: u8,
        value: u8,
    },
    Button {
        transaction: u8,
        control: RawControlId,
        state: ButtonState,
    },
    Rotation {
        transaction: u8,
        control: RawControlId,
        delta: i8,
    },
    Touch(Touch),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputError {
    TooShort,
    LengthMismatch { declared: usize, actual: usize },
    InvalidMessageLength { opcode: u8, actual: usize },
    UnsupportedOpcode(u8),
    InvalidButton(u8),
    InvalidButtonState(u8),
    InvalidRotation(u8),
}

/// Decode input only; identity, firmware and display replies are not input.
pub fn decode_input(payload: &[u8]) -> Result<InputEvent, InputError> {
    if payload.len() < 3 {
        return Err(InputError::TooShort);
    }
    if usize::from(payload[0]) != payload.len() {
        return Err(InputError::LengthMismatch {
            declared: usize::from(payload[0]),
            actual: payload.len(),
        });
    }
    let opcode = payload[1];
    let transaction = payload[2];
    match (opcode, payload.len()) {
        (0x00, 4) => Ok(InputEvent::Tick {
            transaction,
            value: payload[3],
        }),
        (0x00, 5) => {
            let control =
                RawControlId::new(payload[3]).ok_or(InputError::InvalidButton(payload[3]))?;
            let state = match payload[4] {
                0 => ButtonState::Down,
                1 => ButtonState::Up,
                state => return Err(InputError::InvalidButtonState(state)),
            };
            Ok(InputEvent::Button {
                transaction,
                control,
                state,
            })
        }
        (0x01, 5) => {
            if payload[3] > 6 {
                return Err(InputError::InvalidRotation(payload[3]));
            }
            let control =
                RawControlId::new(payload[3]).expect("all seven rotating control IDs are valid");
            Ok(InputEvent::Rotation {
                transaction,
                control,
                delta: i8::from_ne_bytes([payload[4]]),
            })
        }
        (0x4d | 0x6d | 0x52 | 0x72, 9) => {
            let (family, phase) = match opcode {
                0x4d => (TouchFamily::Standard, TouchPhase::Contact),
                0x6d => (TouchFamily::Standard, TouchPhase::End),
                0x52 => (TouchFamily::Ct, TouchPhase::Contact),
                0x72 => (TouchFamily::Ct, TouchPhase::End),
                _ => unreachable!(),
            };
            Ok(InputEvent::Touch(Touch {
                transaction,
                family,
                phase,
                unknown: payload[3],
                x: u16::from_be_bytes([payload[4], payload[5]]),
                y: u16::from_be_bytes([payload[6], payload[7]]),
                contact_id: payload[8],
            }))
        }
        (0x00 | 0x01 | 0x4d | 0x6d | 0x52 | 0x72, actual) => {
            Err(InputError::InvalidMessageLength { opcode, actual })
        }
        _ => Err(InputError::UnsupportedOpcode(opcode)),
    }
}
