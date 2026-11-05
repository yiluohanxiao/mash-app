//! Event-map viewport geometry: panning, zooming, boundary detection,
//! and label identity.
//!
//! The map is a 2-D scrollable surface whose edges are empty terrain
//! with no labels. "Did the view move?" therefore cannot be answered by
//! label changes; the runtime compares downscaled frames instead.
//! Zoom level is not remembered by the game, so every return to the map
//! has to re-run [`MapNavigator::zoom_out`].

use crate::screen::MapLabel;
use crate::touch::TouchBackend;

/// Resolution the reference constants were measured at.
const REFERENCE: (u32, u32) = (1920, 1080);
/// Horizontal pan distance (reference px).
const PAN_STEP_REFERENCE: u32 = 1400;
/// Horizontal pan row, below the top bar and above the bottom UI.
const PAN_Y_REFERENCE: u32 = 600;
/// Vertical pan endpoints (reference px): biased low so the drag never
/// starts on the top bar or ends on the bottom buttons.
const PAN_Y_FROM_REFERENCE: u32 = 750;
const PAN_Y_TO_REFERENCE: u32 = 300;
const PAN_MS: u32 = 800;
const PAN_SETTLE_MS: u32 = 180;
/// Pinch distance between the two contacts (reference px, total gap).
const ZOOM_FROM_GAP_REFERENCE: u32 = 600;
const ZOOM_TO_GAP_REFERENCE: u32 = 120;
const ZOOM_MS: u32 = 300;
/// Mean thumbnail difference below which a pan is considered blocked.
const BOUNDARY_DIFF: f64 = 3.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Axis {
    X,
    Y,
}

fn scaled(value: u32, reference: u32, screen: u32) -> u32 {
    ((u64::from(value) * u64::from(screen) / u64::from(reference)) as u32).max(1)
}

/// Swipe endpoints for one map pan.
///
/// `direction < 0` drags the map left/up (revealing the right/bottom);
/// `direction > 0` drags right/down (revealing the left/top).
pub(crate) fn pan_swipe(
    direction: i32,
    axis: Axis,
    screen: (u32, u32),
) -> ((u32, u32), (u32, u32)) {
    let center_x = screen.0 / 2;
    match axis {
        Axis::X => {
            let y = scaled(PAN_Y_REFERENCE, REFERENCE.1, screen.1).min(screen.1.saturating_sub(1));
            let half = (scaled(PAN_STEP_REFERENCE, REFERENCE.0, screen.0) / 2)
                .min(center_x.saturating_sub(1))
                .max(1);
            if direction < 0 {
                ((center_x + half, y), (center_x - half, y))
            } else {
                ((center_x - half, y), (center_x + half, y))
            }
        }
        Axis::Y => {
            let x = center_x;
            let from = scaled(PAN_Y_FROM_REFERENCE, REFERENCE.1, screen.1).min(screen.1.saturating_sub(1));
            let to = scaled(PAN_Y_TO_REFERENCE, REFERENCE.1, screen.1).min(screen.1.saturating_sub(1));
            if direction < 0 {
                ((x, from), (x, to))
            } else {
                ((x, to), (x, from))
            }
        }
    }
}

/// Mean absolute difference between two thumbnails (0..=255).
pub(crate) fn mean_abs_diff(a: &[u8], b: &[u8]) -> Option<f64> {
    if a.is_empty() || a.len() != b.len() {
        return None;
    }
    let total: u64 = a
        .iter()
        .zip(b)
        .map(|(left, right)| u64::from(left.abs_diff(*right)))
        .sum();
    Some(total as f64 / a.len() as f64)
}

/// A pan that barely changed the frame has hit the map edge.
pub(crate) fn is_boundary(diff: f64) -> bool {
    diff < BOUNDARY_DIFF
}

/// Labels safe to tap, deduplicated by text. OCR occasionally splits one
/// label into two fragments; keeping the most confident read avoids
/// opening the same card twice under two spellings.
pub(crate) fn usable_labels(labels: &[MapLabel]) -> Vec<MapLabel> {
    let mut result: Vec<MapLabel> = Vec::new();
    for label in labels {
        let text = label.text.trim();
        if text.is_empty() {
            continue;
        }
        let mut label = label.clone();
        label.text = text.to_string();
        match result.iter_mut().find(|existing| existing.text == text) {
            Some(existing) if label.confidence > existing.confidence => *existing = label,
            Some(_) => {}
            None => result.push(label),
        }
    }
    result
}

/// Normalize a label for fuzzy comparison: drop spaces and punctuation
/// that OCR renders inconsistently.
fn normalize_label(text: &str) -> String {
    text.chars()
        .filter(|c| {
            !c.is_whitespace()
                && !matches!(
                    c,
                    '!' | '！' | '?' | '？' | '·' | '、' | ',' | '，' | '.' | '。' | '-'
                        | '—' | '～' | '~' | '「' | '」' | '『' | '』' | '【' | '】'
                )
        })
        .collect()
}

/// Port of the reference `_hint_ok`: OCR reads a region name slightly
/// differently each time, so re-finding a label after closing a card
/// accepts either a containment match or two shared characters.
pub(crate) fn hint_ok(text: &str, hint: &str) -> bool {
    if hint.is_empty() {
        return false;
    }
    let text = normalize_label(text);
    let hint = normalize_label(hint);
    if !text.is_empty() && !hint.is_empty() && (text.contains(&hint) || hint.contains(&text)) {
        return true;
    }
    text.chars()
        .filter(|c| hint.contains(*c))
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        >= 2
}

/// Label center in device pixels.
pub(crate) fn label_point(label: &MapLabel, screen: (u32, u32)) -> (u32, u32) {
    let x = (label.cx.clamp(0.0, 1.0) * f64::from(screen.0.saturating_sub(1))).round() as u32;
    let y = (label.cy.clamp(0.0, 1.0) * f64::from(screen.1.saturating_sub(1))).round() as u32;
    (x, y)
}

/// Drives the map through a touch backend.
pub(crate) struct MapNavigator<'a> {
    touch: &'a mut dyn TouchBackend,
    screen: (u32, u32),
}

impl<'a> MapNavigator<'a> {
    pub(crate) fn new(touch: &'a mut dyn TouchBackend, screen: (u32, u32)) -> Self {
        Self { touch, screen }
    }

    /// One pan. The caller re-reads the frame to tell whether it moved.
    pub(crate) fn pan(&mut self, direction: i32, axis: Axis) -> Result<(), String> {
        let (from, to) = pan_swipe(direction, axis, self.screen);
        self.touch
            .swipe_with_settle(from, to, PAN_MS, PAN_SETTLE_MS)
    }

    /// One pinch round that zooms the map out one notch.
    pub(crate) fn zoom_out(&mut self) -> Result<(), String> {
        let center = (self.screen.0 / 2, self.screen.1 / 2);
        let from_gap = scaled(ZOOM_FROM_GAP_REFERENCE, REFERENCE.0, self.screen.0);
        let to_gap = scaled(ZOOM_TO_GAP_REFERENCE, REFERENCE.0, self.screen.0);
        self.touch.pinch(center, from_gap, to_gap, ZOOM_MS)
    }
}
