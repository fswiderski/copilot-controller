//! RGB565 conversion, bounded framebuffer placement, and offline previews.

pub const MAX_PIXEL_BYTES: usize = 194_400;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    pub const fn rgb565(self) -> u16 {
        ((self.r as u16 >> 3) << 11) | ((self.g as u16 >> 2) << 5) | (self.b as u16 >> 3)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Display {
    Center,
    Left,
    Right,
    Wheel,
}

impl Display {
    pub const fn id(self) -> u8 {
        match self {
            Self::Center => b'A',
            Self::Left => b'L',
            Self::Right => b'R',
            Self::Wheel => b'W',
        }
    }

    pub const fn dimensions(self) -> (u16, u16) {
        match self {
            Self::Center => (360, 270),
            Self::Left | Self::Right => (60, 270),
            Self::Wheel => (240, 240),
        }
    }

    fn encode(self, value: u16) -> [u8; 2] {
        match self {
            Self::Wheel => value.to_be_bytes(),
            _ => value.to_le_bytes(),
        }
    }

    fn decode(self, bytes: [u8; 2]) -> u16 {
        match self {
            Self::Wheel => u16::from_be_bytes(bytes),
            _ => u16::from_le_bytes(bytes),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PixelError {
    InvalidKey(u8),
    InvalidDimensions {
        display: Display,
        width: u16,
        height: u16,
    },
    InvalidPlacement,
    PixelCount {
        expected: usize,
        actual: usize,
    },
    ByteCount {
        expected: usize,
        actual: usize,
    },
    InvalidScale(u8),
    Allocation,
}

fn pixel_count(display: Display, width: u16, height: u16) -> Result<usize, PixelError> {
    let (max_width, max_height) = display.dimensions();
    if width == 0 || height == 0 || width > max_width || height > max_height {
        return Err(PixelError::InvalidDimensions {
            display,
            width,
            height,
        });
    }
    Ok(usize::from(width) * usize::from(height))
}

pub fn encode_rgb565(
    display: Display,
    width: u16,
    height: u16,
    pixels: &[Rgb],
) -> Result<Vec<u8>, PixelError> {
    let count = pixel_count(display, width, height)?;
    if pixels.len() != count {
        return Err(PixelError::PixelCount {
            expected: count,
            actual: pixels.len(),
        });
    }
    let byte_count = count * 2;
    if byte_count > MAX_PIXEL_BYTES {
        return Err(PixelError::InvalidDimensions {
            display,
            width,
            height,
        });
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(byte_count)
        .map_err(|_| PixelError::Allocation)?;
    for pixel in pixels {
        bytes.extend_from_slice(&display.encode(pixel.rgb565()));
    }
    Ok(bytes)
}

pub fn decode_rgb565(
    display: Display,
    width: u16,
    height: u16,
    bytes: &[u8],
) -> Result<Vec<Rgb>, PixelError> {
    let count = pixel_count(display, width, height)?;
    let expected = count * 2;
    if bytes.len() != expected {
        return Err(PixelError::ByteCount {
            expected,
            actual: bytes.len(),
        });
    }
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(count)
        .map_err(|_| PixelError::Allocation)?;
    for pair in bytes.as_chunks::<2>().0 {
        let word = display.decode(*pair);
        let red = ((word >> 11) & 0x1f) as u8;
        let green = ((word >> 5) & 0x3f) as u8;
        let blue = (word & 0x1f) as u8;
        pixels.push(Rgb::new(
            (red << 3) | (red >> 2),
            (green << 2) | (green >> 4),
            (blue << 3) | (blue >> 2),
        ));
    }
    Ok(pixels)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KeyIndex(u8);

impl KeyIndex {
    pub fn new(value: u8) -> Result<Self, PixelError> {
        if value < 12 {
            Ok(Self(value))
        } else {
            Err(PixelError::InvalidKey(value))
        }
    }

    pub const fn value(self) -> u8 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Region {
    Key(KeyIndex),
    LeftStrip,
    RightStrip,
    Wheel,
}

impl Region {
    pub fn key(index: u8) -> Result<Self, PixelError> {
        Ok(Self::Key(KeyIndex::new(index)?))
    }

    pub const fn dimensions(self) -> (u16, u16) {
        match self {
            Self::Key(_) => (90, 90),
            Self::LeftStrip | Self::RightStrip => (60, 270),
            Self::Wheel => (240, 240),
        }
    }

    pub const fn placement(self) -> Placement {
        match self {
            Self::Key(key) => Placement {
                display: Display::Center,
                x: 90 * (key.0 as u16 % 4),
                y: 90 * (key.0 as u16 / 4),
                width: 90,
                height: 90,
            },
            Self::LeftStrip => Placement {
                display: Display::Left,
                x: 0,
                y: 0,
                width: 60,
                height: 270,
            },
            Self::RightStrip => Placement {
                display: Display::Right,
                x: 0,
                y: 0,
                width: 60,
                height: 270,
            },
            Self::Wheel => Placement {
                display: Display::Wheel,
                x: 0,
                y: 0,
                width: 240,
                height: 240,
            },
        }
    }
}

pub const REGIONS: [Region; 15] = [
    Region::Key(KeyIndex(0)),
    Region::Key(KeyIndex(1)),
    Region::Key(KeyIndex(2)),
    Region::Key(KeyIndex(3)),
    Region::Key(KeyIndex(4)),
    Region::Key(KeyIndex(5)),
    Region::Key(KeyIndex(6)),
    Region::Key(KeyIndex(7)),
    Region::Key(KeyIndex(8)),
    Region::Key(KeyIndex(9)),
    Region::Key(KeyIndex(10)),
    Region::Key(KeyIndex(11)),
    Region::LeftStrip,
    Region::RightStrip,
    Region::Wheel,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    display: Display,
    x: u16,
    y: u16,
    width: u16,
    height: u16,
}

impl Placement {
    pub fn new(
        display: Display,
        x: u16,
        y: u16,
        width: u16,
        height: u16,
    ) -> Result<Self, PixelError> {
        pixel_count(display, width, height)?;
        let (max_width, max_height) = display.dimensions();
        if u32::from(x) + u32::from(width) > u32::from(max_width)
            || u32::from(y) + u32::from(height) > u32::from(max_height)
        {
            return Err(PixelError::InvalidPlacement);
        }
        Ok(Self {
            display,
            x,
            y,
            width,
            height,
        })
    }

    pub const fn display(self) -> Display {
        self.display
    }

    pub const fn x(self) -> u16 {
        self.x
    }

    pub const fn y(self) -> u16 {
        self.y
    }

    pub const fn width(self) -> u16 {
        self.width
    }

    pub const fn height(self) -> u16 {
        self.height
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Framebuffer {
    region: Region,
    generation: u64,
    bytes: Vec<u8>,
}

impl Framebuffer {
    pub fn from_rgb(region: Region, generation: u64, pixels: &[Rgb]) -> Result<Self, PixelError> {
        let placement = region.placement();
        let bytes = encode_rgb565(placement.display, placement.width, placement.height, pixels)?;
        Ok(Self {
            region,
            generation,
            bytes,
        })
    }

    pub fn from_rgb565(
        region: Region,
        generation: u64,
        bytes: Vec<u8>,
    ) -> Result<Self, PixelError> {
        let (width, height) = region.dimensions();
        let expected = usize::from(width) * usize::from(height) * 2;
        if bytes.len() != expected {
            return Err(PixelError::ByteCount {
                expected,
                actual: bytes.len(),
            });
        }
        Ok(Self {
            region,
            generation,
            bytes,
        })
    }

    pub const fn region(&self) -> Region {
        self.region
    }

    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub const fn placement(&self) -> Placement {
        self.region.placement()
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Preview this region at its native dimensions with RGB565 quantization.
    pub fn preview(&self) -> Result<Image, PixelError> {
        let placement = self.placement();
        Ok(Image {
            width: placement.width,
            height: placement.height,
            pixels: decode_rgb565(
                placement.display,
                placement.width,
                placement.height,
                &self.bytes,
            )?,
        })
    }

    pub fn draw_command(&self) -> Result<PreparedCommand, PixelError> {
        prepare_draw(self.placement(), &self.bytes, self.generation)
    }
}

/// A prepared body is not a write or an acknowledgment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedCommand {
    pub opcode: u8,
    pub body: Vec<u8>,
    pub generation: u64,
}

pub fn prepare_draw(
    placement: Placement,
    bytes: &[u8],
    generation: u64,
) -> Result<PreparedCommand, PixelError> {
    let expected = usize::from(placement.width) * usize::from(placement.height) * 2;
    if bytes.len() != expected {
        return Err(PixelError::ByteCount {
            expected,
            actual: bytes.len(),
        });
    }
    let mut body = Vec::new();
    body.try_reserve_exact(10 + expected)
        .map_err(|_| PixelError::Allocation)?;
    body.extend_from_slice(&[0, placement.display.id()]);
    body.extend_from_slice(&placement.x.to_be_bytes());
    body.extend_from_slice(&placement.y.to_be_bytes());
    body.extend_from_slice(&placement.width.to_be_bytes());
    body.extend_from_slice(&placement.height.to_be_bytes());
    body.extend_from_slice(bytes);
    Ok(PreparedCommand {
        opcode: 0x10,
        body,
        generation,
    })
}

pub fn prepare_refresh(display: Display, generation: u64) -> PreparedCommand {
    PreparedCommand {
        opcode: 0x0f,
        body: vec![0, display.id()],
        generation,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub width: u16,
    pub height: u16,
    pub pixels: Vec<Rgb>,
}

fn filled_pixels(count: usize, color: Rgb) -> Result<Vec<Rgb>, PixelError> {
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(count)
        .map_err(|_| PixelError::Allocation)?;
    pixels.resize(count, color);
    Ok(pixels)
}

fn blit(
    canvas: &mut [Rgb],
    canvas_width: usize,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    source: &[Rgb],
) {
    for row in 0..height {
        let dst = (y + row) * canvas_width + x;
        let src = row * width;
        canvas[dst..dst + width].copy_from_slice(&source[src..src + width]);
    }
}

/// Compose buffers in supplied order, showing the quantized device colors.
pub fn overview(framebuffers: &[Framebuffer], scale: u8) -> Result<Image, PixelError> {
    if !(1..=4).contains(&scale) {
        return Err(PixelError::InvalidScale(scale));
    }
    const WIDTH: usize = 480;
    const HEIGHT: usize = 526;
    let mut base = filled_pixels(WIDTH * HEIGHT, Rgb::new(64, 64, 64))?;
    for frame in framebuffers {
        let p = frame.placement();
        let (origin_x, origin_y) = match p.display {
            Display::Center => (60, 0),
            Display::Left => (0, 0),
            Display::Right => (420, 0),
            Display::Wheel => (120, 286),
        };
        let decoded = decode_rgb565(p.display, p.width, p.height, frame.bytes())?;
        blit(
            &mut base,
            WIDTH,
            origin_x + usize::from(p.x),
            origin_y + usize::from(p.y),
            usize::from(p.width),
            usize::from(p.height),
            &decoded,
        );
    }
    if scale == 1 {
        return Ok(Image {
            width: WIDTH as u16,
            height: HEIGHT as u16,
            pixels: base,
        });
    }
    let factor = usize::from(scale);
    let width = WIDTH * factor;
    let height = HEIGHT * factor;
    let mut pixels = filled_pixels(width * height, Rgb::default())?;
    for y in 0..height {
        for x in 0..width {
            pixels[y * width + x] = base[(y / factor) * WIDTH + x / factor];
        }
    }
    Ok(Image {
        width: width as u16,
        height: height as u16,
        pixels,
    })
}

/// Compose one native-size display; untouched pixels use the supplied backdrop.
pub fn display_preview(
    display: Display,
    framebuffers: &[Framebuffer],
    backdrop: Rgb,
) -> Result<Image, PixelError> {
    let (width, height) = display.dimensions();
    let mut pixels = filled_pixels(usize::from(width) * usize::from(height), backdrop)?;
    for frame in framebuffers {
        let p = frame.placement();
        if p.display != display {
            continue;
        }
        let decoded = decode_rgb565(display, p.width, p.height, frame.bytes())?;
        blit(
            &mut pixels,
            usize::from(width),
            usize::from(p.x),
            usize::from(p.y),
            usize::from(p.width),
            usize::from(p.height),
            &decoded,
        );
    }
    Ok(Image {
        width,
        height,
        pixels,
    })
}
