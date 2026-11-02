//! Quest cards as the event map presents them.
//!
//! Card text is parsed by the sidecar (`read_quest_card`); this module
//! owns the derived meaning the planner depends on: which cards are
//! story nodes, which are farming nodes, and how the recommended level
//! ranks them.

use crate::screen::QuestCardResult;

/// AP at or above this is a farming node (reference: `FARM_AP`).
pub(crate) const FARM_AP: u32 = 20;
/// AP at or below this is a story/challenge node (reference: `STORY_AP`).
pub(crate) const STORY_AP: u32 = 10;

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

    /// Failure-blacklist key. Same name at a different level/AP is a
    /// different node and must not share the counter.
    pub(crate) fn bad_key(&self) -> String {
        format!("{}|{}|{}", self.name, self.level_text(), self.ap)
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
        .filter(|card| !card.locked && !card.name.contains("高难度") && card.ap >= FARM_AP)
        .collect();
    pool.sort_by(|a, b| (b.level_key(), b.ap).cmp(&(a.level_key(), a.ap)));
    pool.first().copied()
}
