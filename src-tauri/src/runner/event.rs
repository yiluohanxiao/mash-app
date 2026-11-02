//! Event-farm navigation, card parsing, and scan bookkeeping.
//!
//! The runtime layer (M2) drives the event map: it reads region labels,
//! opens each card, and hands the selected quest to the battle runner.
//! Everything here is pure or touch-only so it can be unit-tested
//! without a device.

mod cache;
mod card;
mod map;
mod scan;
mod scout;

pub(crate) use card::{best_farm_quest, QuestCard};
pub(crate) use scan::{save_cache, EventScanner};

#[cfg(test)]
mod tests;
