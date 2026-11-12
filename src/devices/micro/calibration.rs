use std::fmt;

use serde_json::{Map, Number, Value};

use super::json::{numeric, python_truthy, python_whitespace};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    Up,
    Right,
    Down,
    Left,
}

impl Direction {
    pub const ALL: [Self; 4] = [Self::Up, Self::Right, Self::Down, Self::Left];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Right => "right",
            Self::Down => "down",
            Self::Left => "left",
        }
    }

    pub const fn command_key(self) -> &'static str {
        match self {
            Self::Up => "DPAD_UP",
            Self::Right => "DPAD_RIGHT",
            Self::Down => "DPAD_DOWN",
            Self::Left => "DPAD_LEFT",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Up => 0,
            Self::Right => 1,
            Self::Down => 2,
            Self::Left => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalibrationError {
    InvalidKeys,
    InvalidAngles,
    OverlappingDirections,
    NotHeld,
    MalformedSnapshot,
}

impl fmt::Display for CalibrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidKeys => "Invalid D-pad calibration keys",
            Self::InvalidAngles => "D-pad angles must be finite numbers in [0, 1)",
            Self::OverlappingDirections => {
                "D-pad directions overlap; recalibrate while holding each direction"
            }
            Self::NotHeld => "Hold the stick firmly in the requested direction, then capture again",
            Self::MalformedSnapshot => "Malformed joystick snapshot",
        })
    }
}

impl std::error::Error for CalibrationError {}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Calibration {
    angles: [Option<f64>; 4],
}

pub fn circular_distance(a: f64, b: f64) -> f64 {
    let shifted = a - b + 0.5;
    ((shifted - shifted.floor()) - 0.5).abs()
}

impl Calibration {
    pub fn from_json(value: &Value) -> Result<Self, CalibrationError> {
        let object = value.as_object().ok_or(CalibrationError::InvalidKeys)?;
        if object.keys().any(|key| {
            !Direction::ALL
                .iter()
                .any(|direction| key == direction.name())
        }) {
            return Err(CalibrationError::InvalidKeys);
        }
        let mut angles = [None; 4];
        for direction in Direction::ALL {
            if let Some(value) = object.get(direction.name()) {
                let angle = numeric(value).ok_or(CalibrationError::InvalidAngles)?;
                if !(0.0..1.0).contains(&angle) {
                    return Err(CalibrationError::InvalidAngles);
                }
                angles[direction.index()] = Some(angle);
            }
        }
        let calibration = Self { angles };
        calibration.validate_overlap()?;
        Ok(calibration)
    }

    fn validate_overlap(&self) -> Result<(), CalibrationError> {
        if self.is_complete() {
            for left in 0..4 {
                for right in (left + 1)..4 {
                    if circular_distance(
                        self.angles[left].expect("complete calibration"),
                        self.angles[right].expect("complete calibration"),
                    ) < 0.15
                    {
                        return Err(CalibrationError::OverlappingDirections);
                    }
                }
            }
        }
        Ok(())
    }

    pub fn is_complete(&self) -> bool {
        self.angles.iter().all(Option::is_some)
    }

    pub fn angle(&self, direction: Direction) -> Option<f64> {
        self.angles[direction.index()]
    }

    pub fn to_json(&self) -> Value {
        let mut object = Map::new();
        for direction in [
            Direction::Down,
            Direction::Left,
            Direction::Right,
            Direction::Up,
        ] {
            if let Some(angle) = self.angle(direction) {
                object.insert(
                    direction.name().to_owned(),
                    Value::Number(Number::from_f64(angle).expect("validated finite angle")),
                );
            }
        }
        Value::Object(object)
    }

    pub fn nearest(&self, angle: f64) -> Option<Direction> {
        if !self.is_complete() || !angle.is_finite() || !(0.0..1.0).contains(&angle) {
            return None;
        }
        let mut best = Direction::Up;
        let mut best_distance = circular_distance(angle, self.angle(best)?);
        for direction in [Direction::Right, Direction::Down, Direction::Left] {
            let distance = circular_distance(angle, self.angle(direction)?);
            if distance < best_distance {
                best = direction;
                best_distance = distance;
            }
        }
        Some(best)
    }

    pub fn capture(
        &self,
        status: &Value,
        direction: Direction,
        wall_now: f64,
    ) -> Result<Self, CalibrationError> {
        let status = status
            .as_object()
            .ok_or(CalibrationError::MalformedSnapshot)?;
        let empty = Map::new();
        let joystick = match status.get("joystick") {
            None => &empty,
            Some(value) if !python_truthy(value) => &empty,
            Some(Value::Object(object)) => object,
            Some(_) => return Err(CalibrationError::MalformedSnapshot),
        };
        let arithmetic = |value: &Value| -> Result<f64, CalibrationError> {
            match value {
                Value::Bool(value) => Ok(u8::from(*value) as f64),
                Value::Number(number) => number
                    .to_string()
                    .parse::<f64>()
                    .map_err(|_| CalibrationError::MalformedSnapshot),
                _ => Err(CalibrationError::MalformedSnapshot),
            }
        };
        let timestamp = joystick.get("timestamp").map_or(Ok(0.0), arithmetic)?;
        if wall_now - timestamp > 2.0 {
            return Err(CalibrationError::NotHeld);
        }
        let distance = joystick.get("distance").map_or(Ok(0.0), arithmetic)?;
        if distance < 0.65 {
            return Err(CalibrationError::NotHeld);
        }
        let angle = joystick
            .get("angle")
            .and_then(numeric)
            .filter(|angle| (0.0..1.0).contains(angle))
            .ok_or(CalibrationError::InvalidAngles)?;
        let mut result = self.clone();
        result.angles[direction.index()] = Some(angle);
        result.validate_overlap()?;
        Ok(result)
    }
}

pub fn calibration_authorized(answer: &str) -> bool {
    answer == "CALIBRATE"
}

pub fn calibration_quit(answer: &str) -> bool {
    answer
        .trim_matches(python_whitespace)
        .eq_ignore_ascii_case("q")
}
