//! Bounded display-text sanitation, not redaction or guard input.

use unicode_general_category::{GeneralCategory, get_general_category};

const _: () = match unicode_general_category::UNICODE_VERSION {
    (16, 0, 0) => (),
    _ => panic!("CT text preparation requires Unicode 16.0.0 categories"),
};

const SCAN_LIMIT: usize = 256;
const CLUSTER_LIMIT: usize = 40;
const CHAIN_LIMIT: u8 = 16;
const MARK_LIMIT: u8 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    LeftToRight,
    RightToLeft,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedText {
    pub text: String,
    pub truncated: bool,
    pub substituted: bool,
    pub direction: Direction,
}

#[derive(Debug)]
struct Cluster {
    text: String,
    marks: u8,
    chain: u8,
    ends_zwj: bool,
    regional_count: u8,
}

impl Cluster {
    fn new(ch: char, is_mark: bool, is_regional: bool) -> Self {
        Self {
            text: ch.to_string(),
            marks: u8::from(is_mark),
            chain: 1,
            ends_zwj: ch == '\u{200d}',
            regional_count: u8::from(is_regional),
        }
    }

    fn space() -> Self {
        Self::new(' ', false, false)
    }
}

fn is_mark(category: GeneralCategory) -> bool {
    matches!(
        category,
        GeneralCategory::NonspacingMark
            | GeneralCategory::SpacingMark
            | GeneralCategory::EnclosingMark
    )
}

fn is_letter(category: GeneralCategory) -> bool {
    matches!(
        category,
        GeneralCategory::UppercaseLetter
            | GeneralCategory::LowercaseLetter
            | GeneralCategory::TitlecaseLetter
            | GeneralCategory::ModifierLetter
            | GeneralCategory::OtherLetter
    )
}

fn is_separator_or_control(category: GeneralCategory) -> bool {
    matches!(
        category,
        GeneralCategory::Control
            | GeneralCategory::SpaceSeparator
            | GeneralCategory::LineSeparator
            | GeneralCategory::ParagraphSeparator
    )
}

fn direction(text: &str) -> Direction {
    for ch in text.chars() {
        if is_letter(get_general_category(ch)) {
            let code = u32::from(ch);
            return if (0x0590..=0x08ff).contains(&code)
                || (0xfb1d..=0xfdff).contains(&code)
                || (0xfe70..=0xfeff).contains(&code)
                || (0x10800..=0x10fff).contains(&code)
                || (0x1e800..=0x1efff).contains(&code)
            {
                Direction::RightToLeft
            } else {
                Direction::LeftToRight
            };
        }
    }
    Direction::LeftToRight
}

/// Keep at most `min(max_clusters, 40)` approximate clusters; zero means one.
pub fn prepare_text(input: &str, max_clusters: usize) -> PreparedText {
    let limit = max_clusters.clamp(1, CLUSTER_LIMIT);
    let mut chars = input.chars();
    let mut clusters: Vec<Cluster> = Vec::new();
    let mut pending_space = false;
    let mut substituted = false;

    for _ in 0..SCAN_LIMIT {
        let Some(mut ch) = chars.next() else {
            break;
        };
        let mut category = get_general_category(ch);
        if is_separator_or_control(category) {
            if ch != ' ' || pending_space || clusters.is_empty() {
                substituted = true;
            }
            pending_space = true;
            continue;
        }
        if category == GeneralCategory::Format {
            if matches!(ch, '\u{200c}' | '\u{200d}') && !clusters.is_empty() && !pending_space {
                let last = clusters.last_mut().expect("checked nonempty");
                if last.chain < CHAIN_LIMIT {
                    last.text.push(ch);
                    last.chain += 1;
                    last.ends_zwj = ch == '\u{200d}';
                    last.regional_count = 0;
                } else {
                    clusters.push(Cluster::new(ch, false, false));
                }
            } else {
                substituted = true;
            }
            continue;
        }
        if matches!(
            category,
            GeneralCategory::PrivateUse | GeneralCategory::Unassigned | GeneralCategory::Surrogate
        ) {
            ch = '\u{fffd}';
            category = get_general_category(ch);
            substituted = true;
        }
        if pending_space {
            if !clusters.is_empty() {
                clusters.push(Cluster::space());
            }
            pending_space = false;
        }

        let mark = is_mark(category);
        let modifier = ('\u{1f3fb}'..='\u{1f3ff}').contains(&ch);
        let regional = ('\u{1f1e6}'..='\u{1f1ff}').contains(&ch);
        let attach = clusters.last().is_some_and(|last| {
            last.text != " "
                && (mark || modifier || last.ends_zwj || (regional && last.regional_count == 1))
        });
        if attach {
            let last = clusters.last_mut().expect("attach requires a cluster");
            if mark {
                if last.marks == MARK_LIMIT {
                    substituted = true;
                    continue;
                }
                last.marks += 1;
                last.text.push(ch);
                last.regional_count = 0;
            } else if (modifier || last.ends_zwj) && last.chain >= CHAIN_LIMIT {
                clusters.push(Cluster::new(ch, false, regional));
            } else {
                last.text.push(ch);
                last.chain += 1;
                last.ends_zwj = false;
                last.regional_count = if regional && last.regional_count == 1 {
                    2
                } else {
                    0
                };
            }
        } else {
            clusters.push(Cluster::new(ch, mark, regional));
        }
    }
    if pending_space {
        substituted = true;
    }
    let scan_exceeded = chars.next().is_some();
    let truncated = scan_exceeded || clusters.len() > limit;
    let needs_ellipsis = truncated && !clusters.is_empty();
    if needs_ellipsis {
        clusters.truncate(limit - 1);
        while clusters.last().is_some_and(|cluster| cluster.text == " ") {
            clusters.pop();
        }
    }
    let mut text = String::new();
    for cluster in clusters {
        text.push_str(&cluster.text);
    }
    if needs_ellipsis {
        text.push('\u{2026}');
    }
    let direction = direction(&text);
    PreparedText {
        text,
        truncated,
        substituted,
        direction,
    }
}
