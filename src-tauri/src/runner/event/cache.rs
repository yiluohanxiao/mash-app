//! Persisted event-map scan cache.
//!
//! A full scan opens every region card (about 1–3 minutes), so the
//! result is reused for three hours. The map changes when the main story
//! advances, hence the TTL; a card's `plus` suffix is part of the cache
//! format, and an older cache without it is rejected outright rather
//! than silently ranking `90++` as `90`.

use super::card::QuestCard;
use std::path::Path;

/// Scans older than this are re-read (reference: 3 hours).
pub(crate) const SCAN_CACHE_MAX_AGE: u64 = 3 * 3600;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ScanCache {
    /// Unix seconds when the scan finished.
    pub scanned_at: u64,
    pub cards: Vec<QuestCard>,
}

impl ScanCache {
    pub(crate) fn new(cards: Vec<QuestCard>, now: u64) -> Self {
        Self {
            scanned_at: now,
            cards,
        }
    }

    pub(crate) fn is_fresh(&self, now: u64, max_age: u64) -> bool {
        now.saturating_sub(self.scanned_at) <= max_age
    }

    pub(crate) fn load(path: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub(crate) fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("failed to create scan cache dir: {error}"))?;
        }
        let text = serde_json::to_string_pretty(self)
            .map_err(|error| format!("failed to serialize scan cache: {error}"))?;
        std::fs::write(path, text).map_err(|error| format!("failed to write scan cache: {error}"))
    }
}
