//! Event-farm navigation, card parsing, and scan bookkeeping.
//!
//! The runtime layer (M2) drives the event map: it reads region labels,
//! opens each card, and hands the selected quest to the battle runner.
//! Everything here is pure or touch-only so it can be unit-tested
//! without a device.

mod cache;
mod card;
mod farm;
mod map;
mod scan;
mod scout;

pub(crate) use card::{best_farm_quest, plan_once, EventPlan, QuestCard};
pub(crate) use farm::{
    pick_target_card, EventFarmPhase, EventFarmRuntime, NODE_FORWARD, QUEST_START_POINT,
};
pub(crate) use map::{hint_ok, label_point, usable_labels};
pub(crate) use scan::{save_cache, EventScanner};

#[cfg(test)]
mod tests;
