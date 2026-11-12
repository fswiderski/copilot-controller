use super::{
    Profile,
    output::{self, OutputReport, PacketError},
};
use jpeg_encoder::{ColorType, Encoder, EncodingError};
use std::fmt;

pub const JPEG_QUALITY: u8 = 90;

#[derive(Debug)]
pub enum RgbError {
    InvalidLength {
        profile: Profile,
        expected: usize,
        actual: usize,
    },
    Encoder(EncodingError),
    EncodedTooLong {
        actual: usize,
    },
}

impl fmt::Display for RgbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLength {
                profile,
                expected,
                actual,
            } => write!(
                f,
                "invalid {} RGB length {actual}; expected {expected}",
                profile.id()
            ),
            Self::Encoder(error) => write!(f, "JPEG encoding failed: {error}"),
            Self::EncodedTooLong { actual } => {
                write!(
                    f,
                    "encoded JPEG length {actual} exceeds {}",
                    output::MAX_JPEG_LEN
                )
            }
        }
    }
}

impl std::error::Error for RgbError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Encoder(error) => Some(error),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub enum ImageReportsError {
    Packet(PacketError),
    Rgb(RgbError),
}

impl fmt::Display for ImageReportsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Packet(error) => error.fmt(f),
            Self::Rgb(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for ImageReportsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Packet(error) => Some(error),
            Self::Rgb(error) => Some(error),
        }
    }
}

/// Return packed RGB in transport orientation, retaining the logical source.
pub fn prepare_rgb(profile: Profile, rgb: &[u8]) -> Result<Vec<u8>, RgbError> {
    let side = usize::from(profile.key_size());
    let expected = side * side * 3;
    if rgb.len() != expected {
        return Err(RgbError::InvalidLength {
            profile,
            expected,
            actual: rgb.len(),
        });
    }
    if profile == Profile::Original {
        return Ok(rgb.to_vec());
    }

    let mut rotated = vec![0; expected];
    for y in 0..side {
        for x in 0..side {
            let source = (y * side + x) * 3;
            let dest_x = side - 1 - y;
            let dest_y = x;
            let destination = (dest_y * side + dest_x) * 3;
            rotated[destination..destination + 3].copy_from_slice(&rgb[source..source + 3]);
        }
    }
    Ok(rotated)
}

/// Encode exact-size logical RGB at quality 90 in the profile's orientation.
pub fn encode_jpeg(profile: Profile, rgb: &[u8]) -> Result<Vec<u8>, RgbError> {
    let prepared = prepare_rgb(profile, rgb)?;
    let mut jpeg = Vec::new();
    Encoder::new(&mut jpeg, JPEG_QUALITY)
        .encode(
            &prepared,
            profile.key_size(),
            profile.key_size(),
            ColorType::Rgb,
        )
        .map_err(RgbError::Encoder)?;
    if jpeg.len() > output::MAX_JPEG_LEN {
        return Err(RgbError::EncodedTooLong { actual: jpeg.len() });
    }
    Ok(jpeg)
}

/// Validate the key before doing any RGB transformation or JPEG encoding.
pub fn rgb_image_reports(
    profile: Profile,
    key: u8,
    rgb: &[u8],
) -> Result<Vec<OutputReport>, ImageReportsError> {
    output::validate_key(key).map_err(ImageReportsError::Packet)?;
    let jpeg = encode_jpeg(profile, rgb).map_err(ImageReportsError::Rgb)?;
    output::frame_jpeg(key, &jpeg).map_err(ImageReportsError::Packet)
}

pub const DIAGNOSTIC_COLORS: [[u8; 3]; 6] = [
    [160, 25, 25],
    [25, 145, 25],
    [25, 25, 160],
    [20, 130, 130],
    [140, 20, 140],
    [150, 130, 20],
];

const DIGIT_ROWS: [[u8; 5]; 6] = [
    [2, 6, 2, 2, 7],
    [7, 1, 7, 4, 7],
    [7, 1, 7, 1, 7],
    [5, 5, 7, 1, 1],
    [7, 4, 7, 1, 7],
    [7, 4, 7, 5, 7],
];

fn set_pixel(rgb: &mut [u8], side: usize, x: usize, y: usize, color: [u8; 3]) {
    let offset = (y * side + x) * 3;
    rgb[offset..offset + 3].copy_from_slice(&color);
}

/// Draw the numbered diagnostic tile upright, before any transport rotation.
pub fn diagnostic_tile_rgb(profile: Profile, key: u8) -> Result<Vec<u8>, PacketError> {
    output::validate_key(key)?;
    let side = usize::from(profile.key_size());
    let center = side / 2;
    let mut rgb = vec![0; side * side * 3];
    for pixel in rgb.as_chunks_mut::<3>().0 {
        *pixel = DIAGNOSTIC_COLORS[usize::from(key)];
    }

    for y in 3..=side - 4 {
        for x in 17..=side - 18 {
            set_pixel(&mut rgb, side, x, y, [0, 0, 0]);
        }
    }
    for y in 5..=12 {
        let half_width = y - 5;
        for x in center - half_width..=center + half_width {
            set_pixel(&mut rgb, side, x, y, [255, 255, 255]);
        }
    }
    for y in 12..=19 {
        for x in center - 2..=center + 2 {
            set_pixel(&mut rgb, side, x, y, [255, 255, 255]);
        }
    }
    for (row, mask) in DIGIT_ROWS[usize::from(key)].into_iter().enumerate() {
        for column in 0..3 {
            if mask & (1 << (2 - column)) != 0 {
                for dy in 0..5 {
                    for dx in 0..5 {
                        set_pixel(
                            &mut rgb,
                            side,
                            center - 7 + column * 5 + dx,
                            26 + row * 5 + dy,
                            [255, 255, 255],
                        );
                    }
                }
            }
        }
    }
    Ok(rgb)
}
