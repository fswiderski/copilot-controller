//! Safe device realizations, without device I/O or implicit qualification.

use serde::{Deserialize, Serialize};

use crate::domain::{ClockSample, ClockSource, MonotonicStamp, RuntimeEpoch, Settings, Status};

use super::{
    artwork,
    evidence::{
        DeviceKind, Evidence, EvidenceError, EvidenceScope, EvidenceUse, PresentationFeature,
    },
    profiles::{
        AKP03_ORIGINAL_PROFILE, AKP03_REV2_PROFILE, CT_PROFILE, DeviceProfile, MICRO_PROFILE,
    },
    semantic::{
        Authority, Availability, EffectRequest, Occupancy, OverlayKind, Selection, SelectionStyle,
        SemanticSlot, SlotSnapshot, Snapshot, Token, TokenStyle, resolve,
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Preferences {
    pub brightness: f64,
    pub reduced_motion: bool,
    pub high_contrast: bool,
    pub show_words: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            brightness: 0.25,
            reduced_motion: false,
            high_contrast: false,
            show_words: true,
        }
    }
}

impl Preferences {
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            brightness: settings.brightness.get(),
            ..Self::default()
        }
    }

    pub fn validate(self) -> Result<(), RealizationError> {
        if self.brightness.is_finite() && (0.0..=1.0).contains(&self.brightness) {
            Ok(())
        } else {
            Err(RealizationError::InvalidBrightness)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectQualification {
    pub feature: PresentationFeature,
    pub token: Token,
    pub evidence: Evidence,
    pub native_id: u64,
    pub native_speed: f64,
    pub smooth_cycle_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectMode {
    Steady,
    StaticEmphasis,
    NativeSlowPulse { native_id: u64, speed: f64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectFallback {
    ReducedMotion,
    EffectUnqualified,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct EffectRealization {
    pub requested: EffectRequest,
    pub mode: EffectMode,
    pub fallback: Option<EffectFallback>,
}

fn micro_scope(scope: &EvidenceScope) -> bool {
    scope.device == DeviceKind::Micro && scope.profile_id.as_deref() == Some(MICRO_PROFILE)
}

fn steady_qualified(
    qualification: &EffectQualification,
    scope: &EvidenceScope,
    token: Token,
) -> bool {
    micro_scope(scope)
        && qualification.feature == PresentationFeature::Steady
        && qualification.token == token
        && qualification.native_speed.is_finite()
        && qualification.native_speed >= 0.0
        && qualification
            .evidence
            .check(scope, EvidenceUse::VisibleOutput)
            .is_ok()
}

fn pulse_qualified(
    qualification: &EffectQualification,
    scope: &EvidenceScope,
    token: Token,
) -> bool {
    micro_scope(scope)
        && qualification.feature == PresentationFeature::SlowPulse
        && qualification.token == token
        && qualification.native_speed.is_finite()
        && qualification.native_speed > 0.0
        && qualification
            .smooth_cycle_ms
            .is_some_and(|period| period >= 1000)
        && qualification
            .evidence
            .check(scope, EvidenceUse::VisibleOutput)
            .is_ok()
}

pub fn negotiate_effect(
    request: EffectRequest,
    scope: &EvidenceScope,
    token: Token,
    preferences: Preferences,
    qualifications: &[EffectQualification],
) -> Result<EffectRealization, RealizationError> {
    preferences.validate()?;
    if request == EffectRequest::Steady {
        return Ok(EffectRealization {
            requested: request,
            mode: EffectMode::Steady,
            fallback: None,
        });
    }
    if preferences.reduced_motion {
        return Ok(EffectRealization {
            requested: request,
            mode: EffectMode::StaticEmphasis,
            fallback: Some(EffectFallback::ReducedMotion),
        });
    }
    if request == EffectRequest::SlowPulse
        && let Some(qualification) = qualifications
            .iter()
            .find(|qualification| pulse_qualified(qualification, scope, token))
    {
        return Ok(EffectRealization {
            requested: request,
            mode: EffectMode::NativeSlowPulse {
                native_id: qualification.native_id,
                speed: qualification.native_speed,
            },
            fallback: None,
        });
    }
    Ok(EffectRealization {
        requested: request,
        mode: EffectMode::StaticEmphasis,
        fallback: Some(EffectFallback::EffectUnqualified),
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Fallback {
    ReducedMotion,
    EffectUnqualified,
    OutputUnqualified,
    WordsRequired,
    WordsUnsupported,
    ArtworkUnrendered,
    BrightnessNotApplied,
    HighContrastNotApplied,
    StaticEmphasisNotApplied,
    EmptyOffIndistinguishable,
    SelectionFrameUnsupported,
    SelectionContrastNotVisible,
    PendingNotShown,
    CursorNotShown,
    AcceptedOverlayNotRendered,
    NeutralAsStale,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct LedEntry {
    pub id: u8,
    pub c: u32,
    pub b: f64,
    pub e: u64,
    pub s: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RealizedSlot {
    pub semantic: SemanticSlot,
    pub rgb: [u8; 3],
    pub style: TokenStyle,
    pub refusal_detail: Option<&'static str>,
    pub selection_style: Option<SelectionStyle>,
    pub effect: EffectRealization,
    pub effective_brightness: Option<f64>,
    pub tile_size: Option<u16>,
    pub border_width: Option<u8>,
    pub led_entry: Option<LedEntry>,
    pub fallbacks: Vec<Fallback>,
}

/// Fields are private so a caller cannot forge a qualified LED frame from a preview.
#[derive(Clone, Debug, Serialize)]
pub struct RealizedDevice {
    device: DeviceKind,
    profile: String,
    slots: [RealizedSlot; 6],
    command_ready: bool,
    device_io_performed: bool,
    synthetic: bool,
}

impl RealizedDevice {
    pub fn device(&self) -> DeviceKind {
        self.device
    }

    pub fn profile(&self) -> &str {
        &self.profile
    }

    pub fn slots(&self) -> &[RealizedSlot; 6] {
        &self.slots
    }

    /// No partial frame is returned. The caller still owns any device transport.
    pub fn qualified_led_frame(&self) -> Option<[LedEntry; 6]> {
        if self.device != DeviceKind::Micro || self.synthetic {
            return None;
        }
        let entries: Vec<_> = self
            .slots
            .iter()
            .map(|slot| slot.led_entry)
            .collect::<Option<_>>()?;
        entries.try_into().ok()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RealizationError {
    InvalidBrightness,
    InvalidScope(EvidenceError),
    ProfileScopeMismatch,
    UnsupportedProfile,
    InvalidTileGeometry,
    SvgOnlyForAjazz,
}

impl std::fmt::Display for RealizationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidBrightness => f.write_str("brightness must be finite and between 0 and 1"),
            Self::InvalidScope(error) => write!(f, "invalid presentation evidence scope: {error}"),
            Self::ProfileScopeMismatch => {
                f.write_str("presentation scope does not match the device profile")
            }
            Self::UnsupportedProfile => f.write_str("unsupported device/profile combination"),
            Self::InvalidTileGeometry => {
                f.write_str("AKP03 tiles must all match the profile's 60 or 64 pixels")
            }
            Self::SvgOnlyForAjazz => f.write_str("logical SVG preview is available only for AKP03"),
        }
    }
}

impl std::error::Error for RealizationError {}

fn push_once(fallbacks: &mut Vec<Fallback>, value: Fallback) {
    if !fallbacks.contains(&value) {
        fallbacks.push(value);
    }
}

fn validate_profile(profile: &DeviceProfile) -> Result<(), RealizationError> {
    match (profile.device, profile.id.as_str(), profile.tile_size) {
        (DeviceKind::Micro, MICRO_PROFILE, None)
        | (DeviceKind::Ct, CT_PROFILE, None)
        | (DeviceKind::Ajazz, AKP03_ORIGINAL_PROFILE, Some(60))
        | (DeviceKind::Ajazz, AKP03_REV2_PROFILE, Some(64)) => Ok(()),
        _ => Err(RealizationError::UnsupportedProfile),
    }
}

fn screen_output_qualified(
    profile: &DeviceProfile,
    scope: &EvidenceScope,
    semantic: SemanticSlot,
) -> bool {
    let qualified = |feature| {
        profile
            .feature(feature)
            .is_some_and(|evidence| evidence.check(scope, EvidenceUse::VisibleOutput).is_ok())
    };
    qualified(PresentationFeature::Steady)
        && qualified(PresentationFeature::Color)
        && qualified(PresentationFeature::Words)
        && (semantic.selection == Selection::None
            || qualified(PresentationFeature::SelectionFrames))
        && (profile.device != DeviceKind::Ajazz
            || semantic.token.style().glyph.is_none()
            || qualified(PresentationFeature::StatusIcons))
}

fn led_entry(
    semantic: SemanticSlot,
    brightness: f64,
    effect: EffectRealization,
    scope: &EvidenceScope,
    qualifications: &[EffectQualification],
) -> Option<LedEntry> {
    let steady = qualifications
        .iter()
        .find(|qualification| steady_qualified(qualification, scope, semantic.token))?;
    let native = match effect.mode {
        EffectMode::NativeSlowPulse { native_id, speed } => qualifications.iter().find(|item| {
            pulse_qualified(item, scope, semantic.token)
                && item.native_id == native_id
                && item.native_speed == speed
        })?,
        EffectMode::Steady | EffectMode::StaticEmphasis => steady,
    };
    let [red, green, blue] = semantic.token.style().rgb;
    Some(LedEntry {
        id: semantic.slot.number() - 1,
        c: (u32::from(red) << 16) | (u32::from(green) << 8) | u32::from(blue),
        b: brightness,
        e: native.native_id,
        s: native.native_speed,
    })
}

fn realize_inner(
    snapshot: &Snapshot,
    now: ClockSample,
    profile: &DeviceProfile,
    scope: &EvidenceScope,
    preferences: Preferences,
    qualifications: &[EffectQualification],
    synthetic: bool,
) -> Result<RealizedDevice, RealizationError> {
    preferences.validate()?;
    validate_profile(profile)?;
    scope
        .validate(false)
        .map_err(RealizationError::InvalidScope)?;
    if !profile.matches_scope(scope) {
        return Err(RealizationError::ProfileScopeMismatch);
    }
    let resolved = resolve(snapshot, now);
    let slots = resolved.map(|semantic| {
        let qualifications = if synthetic { &[][..] } else { qualifications };
        let mut effect = negotiate_effect(
            semantic.effect,
            scope,
            semantic.token,
            preferences,
            qualifications,
        )
        .expect("preferences were checked before resolving slots");
        let mut fallbacks = Vec::new();
        if let Some(reason) = effect.fallback {
            push_once(
                &mut fallbacks,
                match reason {
                    EffectFallback::ReducedMotion => Fallback::ReducedMotion,
                    EffectFallback::EffectUnqualified => Fallback::EffectUnqualified,
                },
            );
        }
        if semantic.overlay == Some(OverlayKind::Accepted) {
            push_once(&mut fallbacks, Fallback::AcceptedOverlayNotRendered);
        }
        if semantic.cursor {
            push_once(&mut fallbacks, Fallback::CursorNotShown);
        }
        let mut style = semantic.token.style();
        let mut brightness = None;
        let mut tile_size = None;
        let mut border_width = None;
        let mut entry = None;
        match profile.device {
            DeviceKind::Micro => {
                let effective = if semantic.selection == Selection::Confirmed {
                    preferences.brightness
                } else {
                    preferences.brightness / 2.0
                };
                brightness = Some(effective);
                if preferences.show_words {
                    push_once(&mut fallbacks, Fallback::WordsUnsupported);
                }
                if style.glyph.is_some() {
                    push_once(&mut fallbacks, Fallback::ArtworkUnrendered);
                }
                if semantic.selection != Selection::None {
                    push_once(&mut fallbacks, Fallback::SelectionFrameUnsupported);
                }
                let selection_visible = effective > 0.0 && style.rgb != [0, 0, 0];
                if semantic.selection == Selection::Confirmed && !selection_visible {
                    push_once(&mut fallbacks, Fallback::SelectionContrastNotVisible);
                }
                if semantic.selection == Selection::Pending {
                    push_once(&mut fallbacks, Fallback::PendingNotShown);
                }
                if matches!(semantic.token, Token::Empty | Token::Off) {
                    push_once(&mut fallbacks, Fallback::EmptyOffIndistinguishable);
                }
                if preferences.high_contrast {
                    push_once(&mut fallbacks, Fallback::HighContrastNotApplied);
                }
                if effect.mode == EffectMode::StaticEmphasis
                    && (semantic.selection != Selection::Confirmed || !selection_visible)
                {
                    effect.mode = EffectMode::Steady;
                    push_once(&mut fallbacks, Fallback::StaticEmphasisNotApplied);
                }
                if !synthetic {
                    entry = led_entry(semantic, effective, effect, scope, qualifications);
                }
                if entry.is_none() {
                    push_once(&mut fallbacks, Fallback::OutputUnqualified);
                }
            }
            DeviceKind::Ajazz | DeviceKind::Ct => {
                if !preferences.show_words {
                    push_once(&mut fallbacks, Fallback::WordsRequired);
                }
                push_once(&mut fallbacks, Fallback::BrightnessNotApplied);
                if synthetic || !screen_output_qualified(profile, scope, semantic) {
                    push_once(&mut fallbacks, Fallback::OutputUnqualified);
                }
                if profile.device == DeviceKind::Ajazz {
                    tile_size = profile.tile_size;
                    border_width = Some(if preferences.high_contrast { 4 } else { 2 });
                } else {
                    border_width = Some(2);
                    if style.glyph.is_some() {
                        push_once(&mut fallbacks, Fallback::ArtworkUnrendered);
                    }
                    if semantic.token == Token::Neutral {
                        style = Token::Stale.style();
                        push_once(&mut fallbacks, Fallback::NeutralAsStale);
                    }
                    if preferences.high_contrast {
                        push_once(&mut fallbacks, Fallback::HighContrastNotApplied);
                    }
                    if effect.mode == EffectMode::StaticEmphasis {
                        effect.mode = EffectMode::Steady;
                        push_once(&mut fallbacks, Fallback::StaticEmphasisNotApplied);
                    }
                }
            }
        }
        RealizedSlot {
            semantic,
            rgb: semantic.token.style().rgb,
            style,
            refusal_detail: semantic.refusal_detail(),
            selection_style: semantic.selection.style(),
            effect,
            effective_brightness: brightness,
            tile_size,
            border_width,
            led_entry: entry,
            fallbacks,
        }
    });
    Ok(RealizedDevice {
        device: profile.device,
        profile: profile.id.clone(),
        slots,
        command_ready: false,
        device_io_performed: false,
        synthetic,
    })
}

/// Realizes supplied provider data; physical evidence remains a caller assertion.
pub fn realize(
    snapshot: &Snapshot,
    now: ClockSample,
    profile: &DeviceProfile,
    scope: &EvidenceScope,
    preferences: Preferences,
    qualifications: &[EffectQualification],
) -> Result<RealizedDevice, RealizationError> {
    realize_inner(
        snapshot,
        now,
        profile,
        scope,
        preferences,
        qualifications,
        false,
    )
}

/// Fixed demonstration data, never eligible for a physical output claim or frame.
pub fn synthetic_demo(
    profile: &DeviceProfile,
    preferences: Preferences,
) -> Result<RealizedDevice, RealizationError> {
    let epoch = RuntimeEpoch::parse_str("11111111-1111-4111-8111-111111111111")
        .expect("the fixed synthetic epoch is valid");
    let now = ClockSample {
        source: ClockSource::LinuxMonotonic,
        stamp: MonotonicStamp {
            epoch,
            ticks: 0,
            frequency: 1000,
        },
    };
    let statuses = [
        Status::Idle,
        Status::Working,
        Status::Complete,
        Status::Waiting,
        Status::Error,
        Status::Unknown,
    ];
    let snapshot = Snapshot {
        epoch,
        generation: 1,
        connected: true,
        slots: std::array::from_fn(|index| SlotSnapshot {
            occupancy: Occupancy::Bound,
            status: statuses[index],
            availability: Availability::Live,
            selection: match index {
                0 => Selection::Confirmed,
                2 => Selection::Pending,
                _ => Selection::None,
            },
            cursor: false,
            authority: Authority::ObserveOnly,
            overlay: None,
        }),
    };
    realize_inner(
        &snapshot,
        now,
        profile,
        &profile.scope,
        preferences,
        &[],
        true,
    )
}

fn rgb(color: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", color[0], color[1], color[2])
}

/// Upright logical AKP03 SVG; it does not perform JPEG rotation or device I/O.
pub fn akp03_svg(realized: &RealizedDevice) -> Result<String, RealizationError> {
    if realized.device != DeviceKind::Ajazz {
        return Err(RealizationError::SvgOnlyForAjazz);
    }
    let expected = match realized.profile.as_str() {
        AKP03_ORIGINAL_PROFILE => 60,
        AKP03_REV2_PROFILE => 64,
        _ => return Err(RealizationError::InvalidTileGeometry),
    };
    if realized
        .slots
        .iter()
        .any(|slot| slot.tile_size != Some(expected))
    {
        return Err(RealizationError::InvalidTileGeometry);
    }
    let mut svg = String::from(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"240\" height=\"190\" viewBox=\"0 0 240 190\"><defs>",
    );
    for artwork in artwork::ARTWORK {
        svg.push_str(artwork.svg);
    }
    svg.push_str("</defs>");
    for (index, slot) in realized.slots.iter().enumerate() {
        let x = 10 + 76 * (index % 3);
        let y = 10 + 85 * (index / 3);
        let size = usize::from(expected);
        let (border, dashed) = if let Some(selection) = slot.selection_style {
            (selection.rgb, selection.dashed)
        } else if slot.effect.mode == EffectMode::StaticEmphasis {
            (slot.style.fill, false)
        } else {
            ([80, 80, 80], false)
        };
        let width = slot.border_width.unwrap_or(2);
        let dash = if dashed {
            " stroke-dasharray=\"5 3\""
        } else {
            ""
        };
        if slot.selection_style.is_some() || slot.effect.mode == EffectMode::StaticEmphasis {
            svg.push_str(&format!(
                "<rect x=\"{x}\" y=\"{y}\" width=\"{size}\" height=\"{size}\" fill=\"none\" stroke=\"#000000\" stroke-width=\"{}\"{dash}/>",
                width + 2,
            ));
        }
        svg.push_str(&format!(
            "<g><rect x=\"{x}\" y=\"{y}\" width=\"{size}\" height=\"{size}\" fill=\"{}\" stroke=\"{}\" stroke-width=\"{width}\"{dash}/>",
            rgb(slot.style.fill),
            rgb(border),
        ));
        let text = rgb(slot.style.text);
        svg.push_str(&format!(
            "<text x=\"{}\" y=\"{}\" text-anchor=\"middle\" font-size=\"7\" fill=\"{text}\">Slot {}</text>",
            x + size / 2,
            y + 9,
            slot.semantic.slot.number(),
        ));
        if let Some(glyph) = slot.style.glyph {
            svg.push_str(&format!(
                "<use href=\"#{glyph}\" x=\"{}\" y=\"{}\" width=\"21\" height=\"21\" color=\"{text}\"/>",
                x + (size - 21) / 2,
                y + 12,
            ));
        }
        if let Some(detail) = slot.refusal_detail {
            svg.push_str(&format!(
                "<text x=\"{}\" y=\"{}\" text-anchor=\"middle\" font-size=\"6\" fill=\"{text}\">{detail}</text>",
                x + size / 2,
                y + 28,
            ));
        }
        let status_y = y + size - 13;
        if width == 4 {
            svg.push_str(&format!(
                "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"11\" fill=\"#ffffff\"/>",
                x + 3,
                status_y - 8,
                size - 6,
            ));
        }
        let status_color = if width == 4 { "#000000" } else { &text };
        svg.push_str(&format!(
            "<text x=\"{}\" y=\"{status_y}\" text-anchor=\"middle\" font-size=\"7\" fill=\"{status_color}\">{}</text>",
            x + size / 2,
            slot.style.word,
        ));
        if let Some(selection) = slot.selection_style {
            svg.push_str(&format!(
                "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"9\" fill=\"#000000\"/>",
                x + 3,
                y + size - 11,
                size - 6,
            ));
            svg.push_str(&format!(
                "<text x=\"{}\" y=\"{}\" text-anchor=\"middle\" font-size=\"6\" fill=\"{}\">{}</text>",
                x + size / 2,
                y + size - 4,
                rgb(selection.rgb),
                selection.word,
            ));
        }
        svg.push_str("</g>");
    }
    svg.push_str("</svg>");
    Ok(svg)
}
