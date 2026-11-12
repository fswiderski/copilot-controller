use super::super::pixels::{Region, Rgb, decode_rgb565};
use super::super::scene::{
    ActionGlyph, Alignment, Coverage, Icon, Operation, Rasterizer, Rect, RenderError, Scene,
    SceneError, Text, Weight, changed_scenes, render_framebuffer, render_rgb, render_scenes,
};
use super::super::text::Direction;

#[derive(Clone, Copy)]
enum Mode {
    Opaque,
    Pattern,
    WrongWidth,
    WrongLength,
    Fail,
}

struct FakeRasterizer {
    mode: Mode,
    calls: usize,
}

impl FakeRasterizer {
    fn coverage(&mut self, rect: Rect) -> Result<Coverage, &'static str> {
        self.calls += 1;
        if matches!(self.mode, Mode::Fail) {
            return Err("unavailable synthetic backend");
        }
        let mut alpha = vec![255; usize::from(rect.width) * usize::from(rect.height)];
        if matches!(self.mode, Mode::Pattern) {
            for row in alpha.chunks_exact_mut(usize::from(rect.width)) {
                for (x, value) in row.iter_mut().enumerate() {
                    *value = [0, 128, 255][x % 3];
                }
            }
        }
        if matches!(self.mode, Mode::WrongLength) {
            alpha.pop();
        }
        Ok(Coverage {
            width: rect.width + u16::from(matches!(self.mode, Mode::WrongWidth)),
            height: rect.height,
            alpha,
        })
    }
}

impl Rasterizer for FakeRasterizer {
    type Error = &'static str;

    fn text(&mut self, text: &Text) -> Result<Coverage, Self::Error> {
        self.coverage(text.rect)
    }

    fn icon(&mut self, icon: &Icon) -> Result<Coverage, Self::Error> {
        self.coverage(icon.rect)
    }
}

fn text() -> Text {
    Text {
        rect: Rect::new(2, 3, 3, 6),
        text: "x".to_owned(),
        color: Rgb::new(210, 130, 250),
        font_size: 6,
        weight: Weight::Normal,
        line_height: 6,
        lines: 1,
        alignment: Alignment::Center,
        direction: Direction::LeftToRight,
    }
}

fn scene(operations: Vec<Operation>) -> Scene {
    Scene {
        region: Region::key(0).unwrap(),
        background: Rgb::new(10, 30, 50),
        operations,
    }
}

#[test]
fn coverage_blends_rounds_and_cannot_paint_outside_its_rectangle() {
    let scene = scene(vec![Operation::Text(text())]);
    let mut rasterizer = FakeRasterizer {
        mode: Mode::Pattern,
        calls: 0,
    };
    let image = render_rgb(&scene, &mut rasterizer).unwrap();
    let at = |x: usize, y: usize| image.pixels[y * 90 + x];
    assert_eq!(at(2, 3), Rgb::new(10, 30, 50));
    assert_eq!(at(3, 3), Rgb::new(110, 80, 150));
    assert_eq!(at(4, 3), Rgb::new(210, 130, 250));
    assert_eq!(at(1, 3), scene.background);
    assert_eq!(at(5, 3), scene.background);
    assert_eq!(at(4, 9), scene.background);
    assert_eq!(rasterizer.calls, 1);
    let frame = render_framebuffer(&scene, &mut rasterizer, 812).unwrap();
    assert_eq!(frame.generation(), 812);
    assert_eq!(frame.region(), scene.region);
    let pixels =
        decode_rgb565(super::super::pixels::Display::Center, 90, 90, frame.bytes()).unwrap();
    assert_eq!(pixels[3 * 90 + 4], Rgb::new(214, 130, 255));
}

#[test]
fn invalid_operations_are_rejected_before_backend_calls() {
    let invalid = [
        (
            Operation::Fill {
                rect: Rect::new(0, 0, 0, 1),
                color: Rgb::default(),
            },
            SceneError::Rectangle(1),
        ),
        (
            Operation::Fill {
                rect: Rect::new(u16::MAX, 0, 1, 1),
                color: Rgb::default(),
            },
            SceneError::Rectangle(1),
        ),
        (
            Operation::Frame {
                rect: Rect::new(0, 0, 20, 20),
                thickness: 0,
                color: Rgb::default(),
                dashed: false,
            },
            SceneError::Frame(1),
        ),
        (
            Operation::Frame {
                rect: Rect::new(0, 0, 20, 20),
                thickness: 11,
                color: Rgb::default(),
                dashed: false,
            },
            SceneError::Frame(1),
        ),
        (
            Operation::Ring {
                inner: 3,
                outer: 3,
                color: Rgb::default(),
                dashed: false,
            },
            SceneError::Ring(1),
        ),
        (
            Operation::Ring {
                inner: 1,
                outer: 46,
                color: Rgb::default(),
                dashed: false,
            },
            SceneError::Ring(1),
        ),
        (Operation::MaskCircle { radius: 46 }, SceneError::Mask(1)),
        (
            Operation::Icon(Icon {
                rect: Rect::new(0, 0, 11, 11),
                glyph: ActionGlyph::Run,
                color: Rgb::default(),
            }),
            SceneError::Icon(1),
        ),
        (
            Operation::Icon(Icon {
                rect: Rect::new(0, 0, 12, 13),
                glyph: ActionGlyph::Run,
                color: Rgb::default(),
            }),
            SceneError::Icon(1),
        ),
    ];
    for (operation, expected) in invalid {
        let scene = scene(vec![Operation::Text(text()), operation]);
        let mut rasterizer = FakeRasterizer {
            mode: Mode::Opaque,
            calls: 0,
        };
        assert_eq!(
            render_rgb(&scene, &mut rasterizer),
            Err(RenderError::InvalidScene(expected))
        );
        assert_eq!(rasterizer.calls, 0);
    }

    let mut bad = text();
    bad.text = String::new();
    assert_eq!(
        scene(vec![Operation::Text(bad.clone())]).validate(),
        Err(SceneError::Text(0))
    );
    bad.text = "a".repeat(1025);
    assert_eq!(
        scene(vec![Operation::Text(bad.clone())]).validate(),
        Err(SceneError::Text(0))
    );
    bad.text = "a".repeat(1024);
    assert!(scene(vec![Operation::Text(bad.clone())]).validate().is_ok());
    bad.text = "\u{1f600}".repeat(512);
    assert!(scene(vec![Operation::Text(bad.clone())]).validate().is_ok());
    bad.text.push('\u{1f600}');
    assert_eq!(
        scene(vec![Operation::Text(bad.clone())]).validate(),
        Err(SceneError::Text(0))
    );
    bad.text = "a".into();
    bad.font_size = 5;
    assert_eq!(
        scene(vec![Operation::Text(bad.clone())]).validate(),
        Err(SceneError::Text(0))
    );
    bad.font_size = 7;
    assert_eq!(
        scene(vec![Operation::Text(bad.clone())]).validate(),
        Err(SceneError::Text(0))
    );
    bad.font_size = 6;
    bad.lines = 2;
    assert_eq!(
        scene(vec![Operation::Text(bad)]).validate(),
        Err(SceneError::Text(0))
    );

    let too_large = Scene {
        region: Region::Wheel,
        background: Rgb::default(),
        operations: vec![Operation::Icon(Icon {
            rect: Rect::new(0, 0, 91, 91),
            glyph: ActionGlyph::Run,
            color: Rgb::default(),
        })],
    };
    assert_eq!(too_large.validate(), Err(SceneError::Icon(0)));
}

#[test]
fn malformed_coverage_or_missing_backend_is_an_error_not_an_empty_render() {
    let scene = scene(vec![Operation::Text(text())]);
    for mode in [Mode::WrongWidth, Mode::WrongLength] {
        let mut rasterizer = FakeRasterizer { mode, calls: 0 };
        assert!(matches!(
            render_rgb(&scene, &mut rasterizer),
            Err(RenderError::Coverage { .. })
        ));
    }
    let mut unavailable = FakeRasterizer {
        mode: Mode::Fail,
        calls: 0,
    };
    assert_eq!(
        render_rgb(&scene, &mut unavailable),
        Err(RenderError::Backend("unavailable synthetic backend"))
    );
}

#[test]
fn complete_scene_batch_is_prevalidated_and_remains_offline() {
    let scenes =
        super::super::layout::Layout::default().scenes(&super::super::layout::View::default());
    let mut backend = FakeRasterizer {
        mode: Mode::Opaque,
        calls: 0,
    };
    let frames = render_scenes(&scenes, &mut backend, 17).unwrap();
    assert_eq!(frames.len(), 15);
    for (frame, scene) in frames.iter().zip(&scenes) {
        assert_eq!(frame.region(), scene.region);
        assert_eq!(frame.generation(), 17);
    }
    assert!(backend.calls > 0);
    let mut fresh_backend = FakeRasterizer {
        mode: Mode::Opaque,
        calls: 0,
    };
    assert_eq!(
        render_scenes(&scenes, &mut fresh_backend, 17).unwrap(),
        frames
    );

    let mut malformed = scenes;
    malformed
        .last_mut()
        .unwrap()
        .operations
        .push(Operation::MaskCircle { radius: 121 });
    backend.calls = 0;
    assert!(matches!(
        render_scenes(&malformed, &mut backend, 18),
        Err(RenderError::InvalidScene(SceneError::Mask(_)))
    ));
    assert_eq!(backend.calls, 0);

    let valid =
        super::super::layout::Layout::default().scenes(&super::super::layout::View::default());
    backend.mode = Mode::Fail;
    assert_eq!(
        render_scenes(&valid, &mut backend, 19),
        Err(RenderError::Backend("unavailable synthetic backend"))
    );
}

#[test]
fn frame_dashes_ring_segments_and_circle_mask_use_pixel_centers() {
    let white = Rgb::new(255, 255, 255);
    let black = Rgb::default();
    let frame = Scene {
        region: Region::key(0).unwrap(),
        background: black,
        operations: vec![Operation::Frame {
            rect: Rect::new(0, 0, 90, 90),
            thickness: 3,
            color: white,
            dashed: true,
        }],
    };
    let mut backend = FakeRasterizer {
        mode: Mode::Opaque,
        calls: 0,
    };
    let image = render_rgb(&frame, &mut backend).unwrap();
    for x in 3..18 {
        let expected = if (x / 6) % 2 == 0 { white } else { black };
        assert_eq!(image.pixels[x], expected, "horizontal dash x={x}");
        assert_eq!(image.pixels[x * 90], expected, "vertical dash y={x}");
    }
    let inset_frame = Scene {
        region: Region::key(0).unwrap(),
        background: black,
        operations: vec![Operation::Frame {
            rect: Rect::new(4, 4, 82, 82),
            thickness: 3,
            color: white,
            dashed: true,
        }],
    };
    let image = render_rgb(&inset_frame, &mut backend).unwrap();
    for coordinate in 8..20 {
        let expected = if (coordinate / 6) % 2 == 0 {
            white
        } else {
            black
        };
        assert_eq!(image.pixels[4 * 90 + coordinate], expected);
        assert_eq!(image.pixels[coordinate * 90 + 4], expected);
    }

    let ring = Scene {
        region: Region::Wheel,
        background: black,
        operations: vec![
            Operation::Ring {
                inner: 112,
                outer: 120,
                color: white,
                dashed: true,
            },
            Operation::MaskCircle { radius: 120 },
        ],
    };
    let image = render_rgb(&ring, &mut backend).unwrap();
    for segment in 0..32 {
        let angle = (f64::from(segment) + 0.5) * std::f64::consts::TAU / 32.0;
        let x = (120.0 + 116.0 * angle.cos()).floor() as usize;
        let y = (120.0 + 116.0 * angle.sin()).floor() as usize;
        assert_eq!(
            image.pixels[y * 240 + x],
            if segment % 2 == 0 { white } else { black },
            "segment {segment}"
        );
    }
    let mask = Scene {
        region: Region::Wheel,
        background: white,
        operations: vec![Operation::MaskCircle { radius: 100 }],
    };
    let image = render_rgb(&mask, &mut backend).unwrap();
    assert_eq!(image.pixels[120 * 240 + 120], white);
    assert_eq!(image.pixels[120 * 240 + 219], white);
    assert_eq!(image.pixels[120 * 240 + 220], black);
    assert_eq!(image.pixels[0], black);
}

#[test]
fn content_comparison_is_pure_not_a_scheduler_or_delivery_acknowledgment() {
    let original = scene(vec![]);
    let mut changed_background = original.clone();
    changed_background.background = Rgb::new(1, 2, 3);
    let mut changed_region = original.clone();
    changed_region.region = Region::key(1).unwrap();
    assert!(
        changed_scenes(
            std::slice::from_ref(&original),
            std::slice::from_ref(&original)
        )
        .unwrap()
        .is_empty()
    );
    assert_eq!(
        changed_scenes(
            std::slice::from_ref(&original),
            &[changed_background.clone(), changed_region.clone()]
        )
        .unwrap(),
        vec![changed_background, changed_region]
    );
    let invalid = scene(vec![Operation::MaskCircle { radius: 46 }]);
    assert_eq!(
        changed_scenes(std::slice::from_ref(&original), &[invalid]),
        Err(SceneError::Mask(0))
    );
    let mut backend = FakeRasterizer {
        mode: Mode::Opaque,
        calls: 0,
    };
    let first = render_framebuffer(&original, &mut backend, 1).unwrap();
    let second = render_framebuffer(&original, &mut backend, 2).unwrap();
    assert_eq!(first.bytes(), second.bytes());
    assert_ne!(first.generation(), second.generation());
}
