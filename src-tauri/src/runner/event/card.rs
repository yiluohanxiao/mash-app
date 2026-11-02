//! Quest cards as the event map presents them.
//!
//! Card text is parsed by the sidecar (`read_quest_card`); this module
//! owns the derived meaning the planner depends on: which cards are
//! story nodes, which are farming nodes, and how the recommended level
//! ranks them.

use crate::screen::QuestCardResult;
use std::collections::HashMap;

/// AP at or above this is a farming node (reference: `FARM_AP`).
pub(crate) const FARM_AP: u32 = 20;
/// AP at or below this is a story/challenge node (reference: `STORY_AP`).
pub(crate) const STORY_AP: u32 = 10;
/// A label that failed this many consecutive quests is skipped for the rest
/// of the run (reference `flow.BAD_LIMIT`, in-memory only).
pub(crate) const BAD_LIMIT: u32 = 2;

/// Whether a label has burned its failure budget for this run.
pub(crate) fn excluded_label(bad: &HashMap<String, u32>, label: &str) -> bool {
    bad.get(label).is_some_and(|count| *count >= BAD_LIMIT)
}

/// A quest card read from the map's card box.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct QuestCard {
    pub name: String,
    pub level: u32,
    /// Number of `+` suffixes on the recommended level: `90++` ranks
    /// above `90+`, which ranks above plain `90`.
    pub plus: u32,
    pub ap: u32,
    pub locked: bool,
    /// Opening-condition text, used to recover the point threshold.
    pub cond: String,
    /// Region label that opened this card.
    pub label: String,
}

impl QuestCard {
    pub(crate) fn from_result(result: QuestCardResult, label: String) -> Self {
        Self {
            name: result.name,
            level: result.level,
            plus: result.plus,
            ap: result.ap,
            locked: result.locked,
            cond: result.cond,
            label,
        }
    }

    pub(crate) fn level_key(&self) -> (u32, u32) {
        (self.level, self.plus)
    }

    pub(crate) fn level_text(&self) -> String {
        format!("{}{}", self.level, "+".repeat(self.plus as usize))
    }

    /// A region label sometimes opens an "area info" panel instead of a
    /// quest card (level 0, AP 0, name = the region). Those must not be
    /// treated as AP-0 story nodes.
    pub(crate) fn is_real_quest(&self) -> bool {
        self.ap > 0
            || self.name.contains('节')
            || self.name.contains("序幕")
            || self.name.contains("关卡")
            || self.name.contains('终')
    }

    /// Story nodes are low-AP and are not high-difficulty challenges.
    /// The name alone cannot decide: an unlocked story card shows the
    /// quest title, while a locked one shows `主线关卡第N节`.
    pub(crate) fn is_story(&self) -> bool {
        if self.name.contains("高难度") || self.name.to_uppercase().contains("LIVE FES") {
            return false;
        }
        self.ap <= STORY_AP
    }

    pub(crate) fn is_farm(&self) -> bool {
        self.ap >= FARM_AP
    }

    /// Event points required to unlock the card, or 0 when unconstrained.
    /// Mirrors the reference `点数达 X 以上` extraction; unit suffixes
    /// such as `万` are deliberately not interpreted, same as upstream.
    pub(crate) fn threshold(&self) -> u32 {
        let Some(start) = self.cond.find("点数达") else {
            return 0;
        };
        let rest = &self.cond[start + "点数达".len()..];
        let Some(end) = rest.find("以上") else {
            return 0;
        };
        rest[..end]
            .chars()
            .filter(char::is_ascii_digit)
            .collect::<String>()
            .parse()
            .unwrap_or(0)
    }
}

/// Highest recommended level among unlocked farming nodes.
///
/// High-difficulty challenge nodes are excluded: they are not the
/// point-farming target. Ties keep map order, matching the reference's
/// stable descending sort.
pub(crate) fn best_farm_quest(cards: &[QuestCard]) -> Option<&QuestCard> {
    let mut pool: Vec<&QuestCard> = cards
        .iter()
        .filter(|card| !card.locked && !card.name.contains("高难度") && card.is_farm())
        .collect();
    pool.sort_by(|a, b| (b.level_key(), b.ap).cmp(&(a.level_key(), a.ap)));
    pool.first().copied()
}

/// What `plan_once` decided to do next (reference `flow.plan_once`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EventPlan {
    /// Grind points on the best farm quest.
    Farm(QuestCard),
    /// Advance the main story.
    Story(QuestCard),
}

/// Decide the next quest from the current cards and event points, mirroring
/// the reference `plan_once`:
///
/// 1. Locked story nodes exist and the cheapest threshold is reached
///    (points included) — enter it; the game unlocks at the threshold.
/// 2. Locked story nodes exist but points are short — grind the best farm
///    quest first.
/// 3. Unlocked story nodes exist — push the lowest-level one.
/// 4. Otherwise — grind the best farm quest.
///
/// Blacklisted labels are excluded before every step; nothing usable
/// returns an error, which stops the run.
pub(crate) fn plan_once(
    cards: &[QuestCard],
    points: Option<u32>,
    bad: &HashMap<String, u32>,
) -> Result<EventPlan, String> {
    let pts = points.unwrap_or(0);
    let usable: Vec<QuestCard> = cards
        .iter()
        .filter(|card| !excluded_label(bad, &card.label))
        .cloned()
        .collect();

    let locked_story: Vec<&QuestCard> = usable
        .iter()
        .filter(|card| card.locked && card.is_story())
        .collect();
    if let Some(target) = locked_story.iter().min_by_key(|card| {
        let threshold = card.threshold();
        (if threshold == 0 { u32::MAX } else { threshold }, card.level_key())
    }) {
        let need = target.threshold();
        if need > 0 && pts < need {
            return best_farm_quest(&usable)
                .cloned()
                .map(EventPlan::Farm)
                .ok_or_else(|| format!("「{}」需要点数 {need}，当前 {pts}，且没有可打的刷点关卡", target.name));
        }
        return Ok(EventPlan::Story((*target).clone()));
    }

    if let Some(target) = usable
        .iter()
        .filter(|card| !card.locked && card.is_story())
        .min_by_key(|card| card.level_key())
    {
        return Ok(EventPlan::Story(target.clone()));
    }

    best_farm_quest(&usable)
        .cloned()
        .map(EventPlan::Farm)
        .ok_or_else(|| "地图上没有可推进的关卡（主线可能已通关，也没有可打的活动本）".into())
}
