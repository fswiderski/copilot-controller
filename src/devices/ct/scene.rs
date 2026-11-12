//! Validated, synchronous scene composition using injected coverage.

use super::pixels::{Framebuffer, Image, PixelError, Region, Rgb};
use super::text::Direction;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    pub const fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    fn within(self, width: u16, height: u16) -> bool {
        self.width > 0
            && self.height > 0
            && u32::from(self.x) + u32::from(self.width) <= u32::from(width)
            && u32::from(self.y) + u32::from(self.height) <= u32::from(height)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alignment {
    Leading,
    Center,
    Trailing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Weight {
    Normal,
    Bold,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Text {
    pub rect: Rect,
    pub text: String,
    pub color: Rgb,
    pub font_size: u8,
    pub weight: Weight,
    pub line_height: u16,
    pub lines: u8,
    pub alignment: Alignment,
    pub direction: Direction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionGlyph {
    Run,
    Accept,
    Stop,
    Fork,
    Send,
    Health,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Icon {
    pub rect: Rect,
    pub glyph: ActionGlyph,
    pub color: Rgb,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Operation {
    Fill {
        rect: Rect,
        color: Rgb,
    },
    Frame {
        rect: Rect,
        thickness: u16,
        color: Rgb,
        dashed: bool,
    },
    Ring {
        inner: u16,
        outer: u16,
        color: Rgb,
        dashed: bool,
    },
    MaskCircle {
        radius: u16,
    },
    Text(Text),
    Icon(Icon),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scene {
    pub region: Region,
    pub background: Rgb,
    pub operations: Vec<Operation>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SceneError {
    Rectangle(usize),
    Frame(usize),
    Ring(usize),
    Mask(usize),
    Text(usize),
    Icon(usize),
}

impl Scene {
    pub fn validate(&self) -> Result<(), SceneError> {
        let (width, height) = self.region.dimensions();
        let radius_limit = width.min(height) / 2;
        for (index, operation) in self.operations.iter().enumerate() {
            match operation {
                Operation::Fill { rect, .. } => {
                    if !rect.within(width, height) {
                        return Err(SceneError::Rectangle(index));
                    }
                }
                Operation::Frame {
                    rect, thickness, ..
                } => {
                    if !rect.within(width, height)
                        || *thickness == 0
                        || u32::from(*thickness) * 2 > u32::from(rect.width.min(rect.height))
                    {
                        return Err(SceneError::Frame(index));
                    }
                }
                Operation::Ring { inner, outer, .. } => {
                    if inner >= outer || *outer > radius_limit {
                        return Err(SceneError::Ring(index));
                    }
                }
                Operation::MaskCircle { radius } => {
                    if *radius == 0 || *radius > radius_limit {
                        return Err(SceneError::Mask(index));
                    }
                }
                Operation::Text(text) => {
                    if !text.rect.within(width, height)
                        || text.text.is_empty()
                        || text.text.encode_utf16().take(1025).count() > 1024
                        || !(6..=64).contains(&text.font_size)
                        || text.line_height < u16::from(text.font_size)
                        || !(1..=4).contains(&text.lines)
                        || u32::from(text.rect.height)
                            != u32::from(text.line_height) * u32::from(text.lines)
                    {
                        return Err(SceneError::Text(index));
                    }
                }
                Operation::Icon(icon) => {
                    if !icon.rect.within(width, height)
                        || icon.rect.width != icon.rect.height
                        || !(12..=90).contains(&icon.rect.width)
                    {
                        return Err(SceneError::Icon(index));
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Coverage {
    pub width: u16,
    pub height: u16,
    pub alpha: Vec<u8>,
}

/// Backends return coverage only; the compositor owns bounds, tint and blending.
pub trait Rasterizer {
    type Error;

    fn text(&mut self, text: &Text) -> Result<Coverage, Self::Error>;
    fn icon(&mut self, icon: &Icon) -> Result<Coverage, Self::Error>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenderError<E> {
    InvalidScene(SceneError),
    Backend(E),
    Coverage {
        expected_width: u16,
        expected_height: u16,
        actual_width: u16,
        actual_height: u16,
        actual_bytes: usize,
    },
    Pixels(PixelError),
    Allocation,
}

fn blend(background: Rgb, foreground: Rgb, alpha: u8) -> Rgb {
    let a = u32::from(alpha);
    let mix = |fg: u8, bg: u8| ((u32::from(fg) * a + u32::from(bg) * (255 - a) + 127) / 255) as u8;
    Rgb::new(
        mix(foreground.r, background.r),
        mix(foreground.g, background.g),
        mix(foreground.b, background.b),
    )
}

fn apply_coverage<E>(
    pixels: &mut [Rgb],
    canvas_width: usize,
    rect: Rect,
    color: Rgb,
    coverage: Coverage,
) -> Result<(), RenderError<E>> {
    let expected = usize::from(rect.width) * usize::from(rect.height);
    if coverage.width != rect.width
        || coverage.height != rect.height
        || coverage.alpha.len() != expected
    {
        return Err(RenderError::Coverage {
            expected_width: rect.width,
            expected_height: rect.height,
            actual_width: coverage.width,
            actual_height: coverage.height,
            actual_bytes: coverage.alpha.len(),
        });
    }
    for y in 0..usize::from(rect.height) {
        for x in 0..usize::from(rect.width) {
            let dst = (usize::from(rect.y) + y) * canvas_width + usize::from(rect.x) + x;
            let alpha = coverage.alpha[y * usize::from(rect.width) + x];
            pixels[dst] = blend(pixels[dst], color, alpha);
        }
    }
    Ok(())
}

fn radial_distance2(x: usize, y: usize, width: usize, height: usize) -> (i64, i64, i64) {
    let dx = 2 * x as i64 + 1 - width as i64;
    let dy = 2 * y as i64 + 1 - height as i64;
    (dx, dy, dx * dx + dy * dy)
}

fn dash_on(dx: i64, dy: i64) -> bool {
    let angle = (dy as f64)
        .atan2(dx as f64)
        .rem_euclid(std::f64::consts::TAU);
    let segment = (angle * (32.0 / std::f64::consts::TAU)).floor() as u8;
    segment.is_multiple_of(2)
}

/// Complete validation precedes all backend calls. No output is delivered.
pub fn render_rgb<R: Rasterizer>(
    scene: &Scene,
    rasterizer: &mut R,
) -> Result<Image, RenderError<R::Error>> {
    scene.validate().map_err(RenderError::InvalidScene)?;
    let (width, height) = scene.region.dimensions();
    let width_usize = usize::from(width);
    let height_usize = usize::from(height);
    let count = width_usize * height_usize;
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(count)
        .map_err(|_| RenderError::Allocation)?;
    pixels.resize(count, scene.background);

    for operation in &scene.operations {
        match operation {
            Operation::Fill { rect, color } => {
                for y in usize::from(rect.y)..usize::from(rect.y + rect.height) {
                    for x in usize::from(rect.x)..usize::from(rect.x + rect.width) {
                        pixels[y * width_usize + x] = *color;
                    }
                }
            }
            Operation::Frame {
                rect,
                thickness,
                color,
                dashed,
            } => {
                let w = usize::from(rect.width);
                let h = usize::from(rect.height);
                let t = usize::from(*thickness);
                for y in 0..h {
                    for x in 0..w {
                        let horizontal = y < t || y >= h - t;
                        let vertical = x < t || x >= w - t;
                        let canvas_x = usize::from(rect.x) + x;
                        let canvas_y = usize::from(rect.y) + y;
                        let paint = if *dashed {
                            (horizontal && (canvas_x / 6).is_multiple_of(2))
                                || (vertical && (canvas_y / 6).is_multiple_of(2))
                        } else {
                            horizontal || vertical
                        };
                        if paint {
                            let index = canvas_y * width_usize + canvas_x;
                            pixels[index] = *color;
                        }
                    }
                }
            }
            Operation::Ring {
                inner,
                outer,
                color,
                dashed,
            } => {
                let inner2 = 4 * i64::from(*inner).pow(2);
                let outer2 = 4 * i64::from(*outer).pow(2);
                for y in 0..height_usize {
                    for x in 0..width_usize {
                        let (dx, dy, distance2) = radial_distance2(x, y, width_usize, height_usize);
                        if distance2 >= inner2 && distance2 < outer2 && (!dashed || dash_on(dx, dy))
                        {
                            pixels[y * width_usize + x] = *color;
                        }
                    }
                }
            }
            Operation::MaskCircle { radius } => {
                let radius2 = 4 * i64::from(*radius).pow(2);
                for y in 0..height_usize {
                    for x in 0..width_usize {
                        let (_, _, distance2) = radial_distance2(x, y, width_usize, height_usize);
                        if distance2 >= radius2 {
                            pixels[y * width_usize + x] = Rgb::default();
                        }
                    }
                }
            }
            Operation::Text(text) => {
                let coverage = rasterizer.text(text).map_err(RenderError::Backend)?;
                apply_coverage(&mut pixels, width_usize, text.rect, text.color, coverage)?;
            }
            Operation::Icon(icon) => {
                let coverage = rasterizer.icon(icon).map_err(RenderError::Backend)?;
                apply_coverage(&mut pixels, width_usize, icon.rect, icon.color, coverage)?;
            }
        }
    }
    Ok(Image {
        width,
        height,
        pixels,
    })
}

pub fn render_framebuffer<R: Rasterizer>(
    scene: &Scene,
    rasterizer: &mut R,
    generation: u64,
) -> Result<Framebuffer, RenderError<R::Error>> {
    let image = render_rgb(scene, rasterizer)?;
    Framebuffer::from_rgb(scene.region, generation, &image.pixels).map_err(RenderError::Pixels)
}

/// Prepare every buffer synchronously, returning no partial batch on failure.
pub fn render_scenes<R: Rasterizer>(
    scenes: &[Scene],
    rasterizer: &mut R,
    generation: u64,
) -> Result<Vec<Framebuffer>, RenderError<R::Error>> {
    for scene in scenes {
        scene.validate().map_err(RenderError::InvalidScene)?;
    }
    let mut frames = Vec::new();
    frames
        .try_reserve_exact(scenes.len())
        .map_err(|_| RenderError::Allocation)?;
    for scene in scenes {
        frames.push(render_framebuffer(scene, rasterizer, generation)?);
    }
    Ok(frames)
}

/// Pure content comparison; this is neither a rate limiter nor a delivery cache.
pub fn changed_scenes(previous: &[Scene], next: &[Scene]) -> Result<Vec<Scene>, SceneError> {
    for scene in previous.iter().chain(next) {
        scene.validate()?;
    }
    Ok(next
        .iter()
        .filter(|scene| !previous.contains(scene))
        .cloned()
        .collect())
}
