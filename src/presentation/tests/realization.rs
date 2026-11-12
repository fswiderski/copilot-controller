use crate::domain::{RefusalReason, Status};

use super::{assertion, complete_scope, sample, snapshot};
use crate::presentation::{
    evidence::{EvidenceError, EvidenceLabel, EvidenceScope, PresentationFeature},
    profiles,
    realization::{
        EffectFallback, EffectMode, EffectQualification, Fallback, Preferences, RealizationError,
        akp03_svg, negotiate_effect, realize, synthetic_demo,
    },
    semantic::{EffectRequest, Occupancy, Overlay, OverlayKind, Selection, Token},
};

fn qualification(
    scope: &EvidenceScope,
    feature: PresentationFeature,
    token: Token,
) -> EffectQualification {
    EffectQualification {
        feature,
        token,
        evidence: assertion(scope, EvidenceLabel::PhysicallyAccepted),
        native_id: 42,
        native_speed: 0.4,
        smooth_cycle_ms: Some(1000),
    }
}

#[test]
fn v02_pulse_requires_exact_physical_scope_token_period_and_positive_speed() {
    let scope = complete_scope(&profiles::micro());
    let good = qualification(&scope, PresentationFeature::SlowPulse, Token::Working);
    let negotiate =
        |scope: &EvidenceScope, token: Token, item: EffectQualification, preferences| {
            negotiate_effect(EffectRequest::SlowPulse, scope, token, preferences, &[item]).unwrap()
        };
    assert_eq!(
        negotiate(&scope, Token::Working, good.clone(), Preferences::default()).mode,
        EffectMode::NativeSlowPulse {
            native_id: 42,
            speed: 0.4
        }
    );
    let mut invalid = Vec::new();
    let mut q = good.clone();
    q.smooth_cycle_ms = Some(999);
    invalid.push(q);
    let mut q = good.clone();
    q.smooth_cycle_ms = None;
    invalid.push(q);
    for speed in [0.0, -0.1, f64::NAN, f64::INFINITY] {
        let mut q = good.clone();
        q.native_speed = speed;
        invalid.push(q);
    }
    let mut q = good.clone();
    q.token = Token::Idle;
    invalid.push(q);
    let mut q = good.clone();
    q.feature = PresentationFeature::Steady;
    invalid.push(q);
    let mut q = good.clone();
    q.evidence.label = EvidenceLabel::NativeObserved;
    invalid.push(q);
    let mut q = good.clone();
    q.evidence.observation_ref = None;
    invalid.push(q);
    let mut q = good.clone();
    q.evidence.scope.mode = Some("different-layer".into());
    invalid.push(q);
    for q in invalid {
        let result = negotiate(&scope, Token::Working, q, Preferences::default());
        assert_eq!(result.mode, EffectMode::StaticEmphasis);
        assert_eq!(result.fallback, Some(EffectFallback::EffectUnqualified));
    }
    let mut incomplete = scope.clone();
    incomplete.firmware = None;
    let mut matching_incomplete = good.clone();
    matching_incomplete.evidence.scope = incomplete.clone();
    assert_eq!(
        negotiate(
            &incomplete,
            Token::Working,
            matching_incomplete,
            Preferences::default()
        )
        .fallback,
        Some(EffectFallback::EffectUnqualified)
    );
    let other = complete_scope(&profiles::ajazz_rev2());
    let mut wrong_device = good.clone();
    wrong_device.evidence.scope = other.clone();
    assert_eq!(
        negotiate(&other, Token::Working, wrong_device, Preferences::default()).fallback,
        Some(EffectFallback::EffectUnqualified)
    );
    let reduced = Preferences {
        reduced_motion: true,
        ..Preferences::default()
    };
    assert_eq!(
        negotiate(&scope, Token::Working, good.clone(), reduced).fallback,
        Some(EffectFallback::ReducedMotion)
    );
    assert_eq!(
        negotiate_effect(
            EffectRequest::Attention,
            &scope,
            Token::Working,
            Preferences::default(),
            std::slice::from_ref(&good)
        )
        .unwrap()
        .fallback,
        Some(EffectFallback::EffectUnqualified)
    );
    let steady = negotiate_effect(
        EffectRequest::Steady,
        &scope,
        Token::Working,
        reduced,
        &[good],
    )
    .unwrap();
    assert_eq!((steady.mode, steady.fallback), (EffectMode::Steady, None));
}

#[test]
fn v03_demo_is_six_synthetic_slots_and_never_qualified() {
    for profile in [
        profiles::micro(),
        profiles::ajazz_original(),
        profiles::ajazz_rev2(),
        profiles::ct(),
    ] {
        let view = synthetic_demo(&profile, Preferences::default()).unwrap();
        assert_eq!(
            view.slots().each_ref().map(|slot| slot.semantic.token),
            [
                Token::Idle,
                Token::Working,
                Token::Complete,
                Token::Waiting,
                Token::Error,
                Token::Unknown
            ]
        );
        assert_eq!(view.slots()[0].semantic.selection, Selection::Confirmed);
        assert_eq!(view.slots()[2].semantic.selection, Selection::Pending);
        assert!(
            view.slots()
                .iter()
                .all(|slot| slot.fallbacks.contains(&Fallback::OutputUnqualified))
        );
        assert!(view.qualified_led_frame().is_none());
        let json = serde_json::to_value(&view).unwrap();
        assert_eq!(json["synthetic"], true);
        assert_eq!(json["device_io_performed"], false);
        assert_eq!(json["command_ready"], false);
        if profile.device == crate::presentation::evidence::DeviceKind::Micro {
            assert_eq!(view.slots()[0].effective_brightness, Some(0.25));
            assert_eq!(view.slots()[1].effective_brightness, Some(0.125));
            assert!(view.slots().iter().all(|slot| slot.led_entry.is_none()));
        }
    }
    let mut profile = profiles::ajazz_rev2();
    profile.scope = complete_scope(&profile);
    let full = profile.scope.clone();
    profile
        .features
        .iter_mut()
        .find(|f| f.feature == PresentationFeature::Steady)
        .unwrap()
        .evidence = assertion(&full, EvidenceLabel::PhysicallyAccepted);
    let synthetic = synthetic_demo(&profile, Preferences::default()).unwrap();
    assert!(
        synthetic
            .slots()
            .iter()
            .all(|slot| slot.fallbacks.contains(&Fallback::OutputUnqualified))
    );
}

#[test]
fn screen_output_requires_exact_evidence_for_each_rendered_feature() {
    for mut profile in [profiles::ajazz_rev2(), profiles::ct()] {
        let scope = complete_scope(&profile);
        let mut source = snapshot();
        source.slots[0].selection = Selection::Confirmed;
        for feature in &mut profile.features {
            feature.evidence = assertion(&scope, EvidenceLabel::PhysicallyAccepted);
        }
        let render = |profile: &profiles::DeviceProfile| {
            realize(
                &source,
                sample(0),
                profile,
                &scope,
                Preferences::default(),
                &[],
            )
            .unwrap()
        };
        assert!(
            render(&profile)
                .slots()
                .iter()
                .all(|slot| !slot.fallbacks.contains(&Fallback::OutputUnqualified))
        );
        for feature in [
            PresentationFeature::Steady,
            PresentationFeature::Color,
            PresentationFeature::Words,
            PresentationFeature::SelectionFrames,
            PresentationFeature::StatusIcons,
        ] {
            for variant in 0..3 {
                let mut missing = profile.clone();
                let item = missing
                    .features
                    .iter_mut()
                    .find(|item| item.feature == feature)
                    .unwrap();
                match variant {
                    0 => item.evidence.label = EvidenceLabel::NativeObserved,
                    1 => item.evidence.scope.firmware = Some("different".into()),
                    _ => item.evidence.observation_ref = None,
                }
                let view = render(&missing);
                let required = feature != PresentationFeature::StatusIcons
                    || profile.device == crate::presentation::evidence::DeviceKind::Ajazz;
                assert_eq!(
                    view.slots()[0]
                        .fallbacks
                        .contains(&Fallback::OutputUnqualified),
                    required,
                    "{:?} {feature:?} variant {variant}",
                    profile.device
                );
                if feature == PresentationFeature::SelectionFrames {
                    assert!(
                        !view.slots()[1]
                            .fallbacks
                            .contains(&Fallback::OutputUnqualified)
                    );
                }
            }
        }
        profile
            .features
            .retain(|item| item.feature == PresentationFeature::Steady);
        assert!(
            render(&profile).slots()[0]
                .fallbacks
                .contains(&Fallback::OutputUnqualified)
        );
    }
}

#[test]
fn confirmed_off_led_cannot_show_brightness_contrast_or_static_emphasis() {
    let profile = profiles::micro();
    let scope = complete_scope(&profile);
    let mut source = snapshot();
    source.slots[0].status = Status::Off;
    source.slots[0].selection = Selection::Confirmed;
    let qualification = qualification(&scope, PresentationFeature::Steady, Token::Off);
    let view = realize(
        &source,
        sample(0),
        &profile,
        &scope,
        Preferences::default(),
        &[qualification],
    )
    .unwrap();
    let slot = &view.slots()[0];
    assert_eq!(slot.led_entry.unwrap().c, 0);
    assert_eq!(slot.effective_brightness, Some(0.25));
    assert!(
        slot.fallbacks
            .contains(&Fallback::SelectionContrastNotVisible)
    );
    assert!(slot.fallbacks.contains(&Fallback::StaticEmphasisNotApplied));
    assert_eq!(slot.effect.mode, EffectMode::Steady);
}

#[test]
fn v03_micro_reports_all_unrepresentable_channels_without_duplicates() {
    let profile = profiles::micro();
    let mut source = snapshot();
    source.slots[0].status = Status::Off;
    source.slots[1].selection = Selection::Pending;
    source.slots[1].cursor = true;
    source.slots[1].overlay = Some(Overlay {
        kind: OverlayKind::Accepted,
        generation: 1,
        started: sample(0),
    });
    source.slots[2].occupancy = Occupancy::Empty;
    let preferences = Preferences {
        high_contrast: true,
        reduced_motion: true,
        ..Preferences::default()
    };
    let view = realize(
        &source,
        sample(1),
        &profile,
        &profile.scope,
        preferences,
        &[],
    )
    .unwrap();
    let slot = &view.slots()[1];
    for fallback in [
        Fallback::ReducedMotion,
        Fallback::OutputUnqualified,
        Fallback::WordsUnsupported,
        Fallback::ArtworkUnrendered,
        Fallback::HighContrastNotApplied,
        Fallback::StaticEmphasisNotApplied,
        Fallback::SelectionFrameUnsupported,
        Fallback::PendingNotShown,
        Fallback::CursorNotShown,
        Fallback::AcceptedOverlayNotRendered,
    ] {
        assert!(slot.fallbacks.contains(&fallback), "{fallback:?}");
    }
    assert_eq!(
        slot.fallbacks
            .iter()
            .filter(|item| **item == Fallback::CursorNotShown)
            .count(),
        1
    );
    assert_eq!(slot.effect.mode, EffectMode::Steady);
    assert!(
        view.slots()[0]
            .fallbacks
            .contains(&Fallback::EmptyOffIndistinguishable)
    );
    assert!(
        view.slots()[2]
            .fallbacks
            .contains(&Fallback::EmptyOffIndistinguishable)
    );
    assert_eq!(view.slots()[0].rgb, view.slots()[2].rgb);
    let zero = synthetic_demo(
        &profile,
        Preferences {
            brightness: 0.0,
            ..Preferences::default()
        },
    )
    .unwrap();
    assert!(
        zero.slots()[0]
            .fallbacks
            .contains(&Fallback::SelectionContrastNotVisible)
    );
    assert!(
        zero.slots()[0]
            .fallbacks
            .contains(&Fallback::StaticEmphasisNotApplied)
    );
}

#[test]
fn v03_screens_keep_words_report_fallbacks_and_akp_geometry() {
    let preferences = Preferences {
        show_words: false,
        high_contrast: true,
        ..Preferences::default()
    };
    for (profile, size) in [
        (profiles::ajazz_original(), 60),
        (profiles::ajazz_rev2(), 64),
    ] {
        let view = synthetic_demo(&profile, preferences).unwrap();
        for slot in view.slots() {
            assert_eq!(slot.tile_size, Some(size));
            assert_eq!(slot.border_width, Some(4));
            assert!(slot.fallbacks.contains(&Fallback::WordsRequired));
            assert!(slot.fallbacks.contains(&Fallback::BrightnessNotApplied));
            assert!(!slot.fallbacks.contains(&Fallback::HighContrastNotApplied));
        }
        let svg = akp03_svg(&view).unwrap();
        assert!(svg.contains("width=\"240\" height=\"190\""));
        for word in [
            "IDLE", "WORKING", "DONE", "WAITING", "ERROR", "UNKNOWN", "SELECTED", "PENDING",
            "Slot 1",
        ] {
            assert!(svg.contains(word), "{word}");
        }
        assert!(svg.contains("stroke-dasharray=\"5 3\""));
        assert!(svg.contains("id=\"state-working\""));
    }
    let profile = profiles::ct();
    let mut source = snapshot();
    source.slots[0].cursor = true;
    let view = realize(
        &source,
        sample(0),
        &profile,
        &profile.scope,
        preferences,
        &[],
    )
    .unwrap();
    for fallback in [
        Fallback::WordsRequired,
        Fallback::BrightnessNotApplied,
        Fallback::ArtworkUnrendered,
        Fallback::HighContrastNotApplied,
        Fallback::CursorNotShown,
        Fallback::OutputUnqualified,
    ] {
        assert!(
            view.slots()[0].fallbacks.contains(&fallback),
            "{fallback:?}"
        );
    }
    assert_eq!(view.slots()[0].border_width, Some(2));
    assert!(
        view.slots()[1]
            .fallbacks
            .contains(&Fallback::StaticEmphasisNotApplied)
    );
    source.connected = false;
    let offline = realize(
        &source,
        sample(0),
        &profile,
        &profile.scope,
        preferences,
        &[],
    )
    .unwrap();
    assert_eq!(offline.slots()[0].semantic.token, Token::Neutral);
    assert_eq!(offline.slots()[0].style.word, "STALE");
    assert!(
        offline.slots()[0]
            .fallbacks
            .contains(&Fallback::NeutralAsStale)
    );
    assert!(matches!(
        akp03_svg(&offline),
        Err(RealizationError::SvgOnlyForAjazz)
    ));
}

#[test]
fn v03_led_frames_require_steady_for_every_token_and_separate_pulse_proof() {
    let profile = profiles::micro();
    let scope = complete_scope(&profile);
    let mut source = snapshot();
    source.slots[0].selection = Selection::Confirmed;
    let tokens = [
        Token::Idle,
        Token::Working,
        Token::Complete,
        Token::Waiting,
        Token::Error,
        Token::Unknown,
    ];
    let mut qualifications: Vec<_> = tokens
        .iter()
        .map(|token| qualification(&scope, PresentationFeature::Steady, *token))
        .collect();
    let mut pulse = qualification(&scope, PresentationFeature::SlowPulse, Token::Idle);
    pulse.native_id = 88;
    pulse.native_speed = 0.7;
    qualifications.push(pulse);
    let view = realize(
        &source,
        sample(0),
        &profile,
        &scope,
        Preferences::default(),
        &qualifications,
    )
    .unwrap();
    let frame = view.qualified_led_frame().unwrap();
    assert_eq!(frame.map(|entry| entry.id), [0, 1, 2, 3, 4, 5]);
    assert_eq!(
        (frame[0].c, frame[0].b, frame[0].e, frame[0].s),
        (0xffffff, 0.25, 88, 0.7)
    );
    assert_eq!(frame[1].c, 0x2070ff);
    assert_eq!(frame[1].b, 0.125);
    qualifications
        .retain(|q| !(q.feature == PresentationFeature::Steady && q.token == Token::Idle));
    let partial = realize(
        &source,
        sample(0),
        &profile,
        &scope,
        Preferences::default(),
        &qualifications,
    )
    .unwrap();
    assert!(partial.slots()[0].led_entry.is_none());
    assert!(
        partial.slots()[0]
            .fallbacks
            .contains(&Fallback::OutputUnqualified)
    );
    assert!(partial.qualified_led_frame().is_none());
}

#[test]
fn v03_preferences_and_scope_fail_closed_and_accepted_overlay_is_not_drawn() {
    let profile = profiles::ajazz_original();
    for brightness in [-0.01, 1.01, f64::NAN, f64::INFINITY] {
        let preferences = Preferences {
            brightness,
            ..Preferences::default()
        };
        assert!(matches!(
            synthetic_demo(&profile, preferences),
            Err(RealizationError::InvalidBrightness)
        ));
    }
    let mut wrong_scope = profile.scope.clone();
    wrong_scope.profile_id = Some("wrong".into());
    assert!(matches!(
        realize(
            &snapshot(),
            sample(0),
            &profile,
            &wrong_scope,
            Preferences::default(),
            &[]
        ),
        Err(RealizationError::ProfileScopeMismatch)
    ));
    let mut invalid_scope = profile.scope.clone();
    invalid_scope.firmware = Some("bad\nfirmware".into());
    assert!(matches!(
        realize(
            &snapshot(),
            sample(0),
            &profile,
            &invalid_scope,
            Preferences::default(),
            &[]
        ),
        Err(RealizationError::InvalidScope(
            EvidenceError::InvalidScopeField("firmware")
        ))
    ));
    let mut source = snapshot();
    source.slots[0].overlay = Some(Overlay {
        kind: OverlayKind::Accepted,
        generation: 1,
        started: sample(0),
    });
    source.slots[1].overlay = Some(Overlay {
        kind: OverlayKind::Refused(RefusalReason::Disabled),
        generation: 1,
        started: sample(0),
    });
    let view = realize(
        &source,
        sample(1),
        &profile,
        &profile.scope,
        Preferences::default(),
        &[],
    )
    .unwrap();
    assert!(
        view.slots()[0]
            .fallbacks
            .contains(&Fallback::AcceptedOverlayNotRendered)
    );
    assert_eq!(view.slots()[1].style.word, "REFUSED");
    assert_eq!(view.slots()[1].refusal_detail, Some("DISABLED"));
    assert!(akp03_svg(&view).unwrap().contains(">DISABLED</text>"));
}
