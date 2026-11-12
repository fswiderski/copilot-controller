use super::super::text::{Direction, prepare_text};

#[test]
fn display_text_vectors_collapse_spaces_remove_formats_and_replace_invalid_categories() {
    let prepared = prepare_text("  a\t\n\u{2028} b  ", 40);
    assert_eq!(prepared.text, "a b");
    assert!(prepared.substituted);
    let prepared = prepare_text("\u{202e}evil\u{2066}x\u{0007}y", 40);
    assert_eq!(prepared.text, "evilx y");
    assert!(prepared.substituted);
    let prepared = prepare_text("\u{e000}\u{0378}", 40);
    assert_eq!(prepared.text, "\u{fffd}\u{fffd}");
    assert!(prepared.substituted);
    let prepared = prepare_text("\u{200d}x \u{200d}y\u{feff}", 40);
    assert_eq!(prepared.text, "x y");
    assert!(prepared.substituted);
    assert_eq!(prepare_text(" \t\n ", 40).text, "");
}

#[test]
fn clusters_limits_ellipsis_marks_joiners_and_scan_cap() {
    assert_eq!(prepare_text("abcdef", 4).text, "abc\u{2026}");
    assert_eq!(prepare_text("ab cd", 4).text, "ab\u{2026}");
    assert_eq!(
        prepare_text("e\u{301}e\u{301}e\u{301}", 2).text,
        "e\u{301}\u{2026}"
    );
    assert_eq!(prepare_text("abc", 0).text, "\u{2026}");
    assert_eq!(prepare_text("x", 0).text, "x");
    let long = prepare_text(&"x".repeat(10_000), 40);
    assert_eq!(long.text, format!("{}\u{2026}", "x".repeat(39)));
    assert!(long.truncated);
    let marked = prepare_text(&format!("a{}", "\u{301}".repeat(50)), 40);
    assert_eq!(marked.text, format!("a{}", "\u{301}".repeat(4)));
    assert!(marked.substituted);
    assert!(!marked.truncated);

    let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}\u{200d}\u{1f466}";
    let prepared = prepare_text(family, 1);
    assert_eq!(prepared.text, family);
    assert!(!prepared.truncated);
    assert_eq!(
        prepare_text("\u{1f1fa}\u{1f1f8}", 1).text,
        "\u{1f1fa}\u{1f1f8}"
    );
    assert_eq!(
        prepare_text("\u{1f1fa}\u{1f1f8}\u{1f1e8}", 1).text,
        "\u{2026}"
    );
    let long_chain = format!("{}x", "\u{1f468}\u{200d}".repeat(20));
    assert!(prepare_text(&long_chain, 1).truncated);

    let scan_exceeded = prepare_text(&format!("a{}", "\u{301}".repeat(256)), 40);
    assert_eq!(
        scan_exceeded.text,
        format!("a{}\u{2026}", "\u{301}".repeat(4))
    );
    assert!(scan_exceeded.truncated);
    assert!(scan_exceeded.substituted);
    let exact_scan_cap = prepare_text(&format!("a{}", "\u{301}".repeat(255)), 40);
    assert!(!exact_scan_cap.truncated);
    assert_eq!(exact_scan_cap.text, format!("a{}", "\u{301}".repeat(4)));
    let no_printable = prepare_text(&"\u{202e}".repeat(300), 40);
    assert_eq!(no_printable.text, "");
    assert!(no_printable.truncated);
}

#[test]
fn paragraph_direction_comes_from_the_first_letter() {
    for (text, expected) in [
        ("\u{05e9}\u{05dc}\u{05d5}\u{05dd}", Direction::RightToLeft),
        ("\u{0639}\u{0631}\u{0628}\u{064a}", Direction::RightToLeft),
        ("123 \u{05d0}", Direction::RightToLeft),
        ("a \u{0639}", Direction::LeftToRight),
        ("123", Direction::LeftToRight),
    ] {
        assert_eq!(prepare_text(text, 40).direction, expected, "{text}");
    }
}
