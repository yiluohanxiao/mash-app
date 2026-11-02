//! Scan bookkeeping: which labels to re-read and how to merge the result.
//!
//! A full scan is expensive, so once the map is known only two kinds of
//! label are re-read:
//! - labels that did not exist in the previous scan (a new node after
//!   main-story progress), and
//! - labels whose card was locked (it may have unlocked; unlocking also
//!   changes the card name).

use super::card::QuestCard;
use crate::screen::MapLabel;

/// Labels to (re)read for a delta scan: new labels first, then locked
/// ones. The returned labels borrow from `labels`.
pub(crate) fn delta_targets<'a>(
    previous: &[QuestCard],
    labels: &'a [MapLabel],
) -> Vec<&'a MapLabel> {
    let known: Vec<&QuestCard> = previous
        .iter()
        .filter(|card| !card.label.is_empty())
        .collect();
    let mut targets: Vec<&MapLabel> = Vec::new();
    for label in labels {
        match known.iter().find(|card| card.label == label.text) {
            None => targets.push(label),
            Some(card) if card.locked => targets.push(label),
            Some(_) => {}
        }
    }
    targets
}

/// Merge a delta read into the previous scan.
///
/// When the same label yields both a locked and an unlocked card (the
/// card was renamed on unlock), keep the unlocked entry and drop the old
/// name, so the planner never targets a stale locked node.
pub(crate) fn merge_delta(
    previous: Vec<QuestCard>,
    added: Vec<QuestCard>,
    targets: &[MapLabel],
) -> Vec<QuestCard> {
    let mut merged = previous;
    for card in added {
        if let Some(existing) = merged.iter_mut().find(|item| item.name == card.name) {
            *existing = card;
        } else {
            merged.push(card);
        }
    }

    for label in targets {
        let entries: Vec<usize> = merged
            .iter()
            .enumerate()
            .filter(|(_, card)| card.label == label.text)
            .map(|(index, _)| index)
            .collect();
        if entries.len() <= 1 {
            continue;
        }
        let keep = entries
            .iter()
            .copied()
            .find(|index| !merged[*index].locked)
            .unwrap_or(entries[0]);
        let mut index = 0;
        merged.retain(|card| {
            let retained = index == keep || card.label != label.text;
            index += 1;
            retained
        });
    }
    merged
}
