//! Single-screen event-map scan.
//!
//! Reference behaviour (`scout.scan`): zoom the map fully out, rewind to
//! the top-left corner, then open every visible region label and read its
//! card. At the minimum zoom the whole map fits on one screen (eight
//! labels in the reference event), so no 2-D walking is needed. A map
//! that still overflows is reported instead of being half-scanned; the
//! 2-D `walk_map` fallback lands with the navigation runtime.

use super::cache::ScanCache;
use super::card::QuestCard;
use super::map::{self, Axis, MapNavigator};
use crate::screen::{EventCvSource, MapLabel, Screen};
use crate::touch::TouchBackend;
use std::thread::sleep;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Wait for the map to finish rendering after entering it.
const MAP_SETTLE: Duration = Duration::from_millis(1_500);
/// Settle window after tapping a region label, shared with the runtime's
/// label tap.
pub(crate) const TAP_SETTLE: Duration = Duration::from_millis(1_800);
const CLOSE_SETTLE: Duration = Duration::from_millis(1_400);
const PAN_SETTLE: Duration = Duration::from_millis(1_600);
const PINCH_SETTLE: Duration = Duration::from_millis(1_400);
const CARD_RETRY_DELAY: Duration = Duration::from_millis(600);
/// Below this many labels the map did not fit on one screen.
const MIN_SCREEN_LABELS: usize = 3;
/// At this many labels the whole map is visible; stop pinching.
const TARGET_LABELS: usize = 7;
const MAX_ZOOM_ROUNDS: u32 = 4;
const MAX_PAN_STEPS: u32 = 12;
/// Whole-map label search: panning steps before giving up. The reference
/// event map is three rows tall at minimum zoom; 48 steps covers it with
/// a wide margin.
const MAX_FIND_STEPS: usize = 48;
const CARD_RETRIES: u32 = 3;
const THUMBNAIL: (u32, u32) = (64, 36);
/// Card "close" button in the top-left, and the empty-space fallback for
/// locked cards that have no close button.
const CARD_CLOSE_POINT: (f64, f64) = (163.0 / 1920.0, 63.0 / 1080.0);
const CARD_CLOSE_FALLBACK: (f64, f64) = (0.5, 700.0 / 1080.0);

pub(crate) struct ScanOutcome {
    pub labels: Vec<String>,
    pub cards: Vec<QuestCard>,
    pub points: Option<u32>,
}

pub(crate) struct EventScanner<'a, S: EventCvSource> {
    client: &'a mut S,
    touch: &'a mut dyn TouchBackend,
    screen: (u32, u32),
}

impl<'a, S: EventCvSource> EventScanner<'a, S> {
    pub(crate) fn new(
        client: &'a mut S,
        touch: &'a mut dyn TouchBackend,
        screen: (u32, u32),
    ) -> Self {
        Self {
            client,
            touch,
            screen,
        }
    }

    /// Scan the currently displayed event map. The caller must already be
    /// on `EventMap`; getting there is the navigation runtime's job.
    pub(crate) fn scan(&mut self) -> Result<ScanOutcome, String> {
        if self.detect()? != Screen::EventMap {
            return Err("当前不在活动地图，无法扫描".into());
        }
        self.zoom_out()?;
        let labels = self.labels()?;
        if labels.len() < MIN_SCREEN_LABELS {
            return Err(format!(
                "缩到最小后只看到 {} 个区域标签（少于 {MIN_SCREEN_LABELS} 个）；\
                 二维拖图扫描将在导航运行时接入",
                labels.len()
            ));
        }

        let mut cards = Vec::new();
        self.visit_labels(&labels, &mut cards)?;
        let points = self
            .client
            .read_event_points(None)
            .ok()
            .and_then(|result| result.points);
        Ok(ScanOutcome {
            labels: labels.into_iter().map(|label| label.text).collect(),
            cards,
            points,
        })
    }

    /// Incremental re-scan after quest progress (reference `scout.scan`
    /// delta mode): only labels that were not in `previous`, or whose card
    /// was locked and may have unlocked since, are opened again. The merged
    /// list keeps every previously known card.
    pub(crate) fn scan_delta(&mut self, previous: &[QuestCard]) -> Result<Vec<QuestCard>, String> {
        if self.detect()? != Screen::EventMap {
            return Err("当前不在活动地图，无法增量扫描".into());
        }
        self.zoom_out()?;
        let targets: Vec<MapLabel> = super::scout::delta_targets(previous, &self.labels()?)
            .into_iter()
            .cloned()
            .collect();
        if targets.is_empty() {
            return Ok(previous.to_vec());
        }
        let mut added = Vec::new();
        self.visit_labels(&targets, &mut added)?;
        Ok(super::scout::merge_delta(previous.to_vec(), added, &targets))
    }

    /// Search the whole map for one region label: zoom out (the game does
    /// not remember zoom between map visits), rewind to the top-left
    /// corner, then sweep row by row — right to the edge, one row down,
    /// left to the edge, … — until the label shows up. Returns the label
    /// as read in the frame where it was found, so its point is tappable
    /// right away.
    pub(crate) fn find_label(&mut self, text: &str) -> Result<Option<MapLabel>, String> {
        self.zoom_out()?;
        self.rewind()?;
        let mut reveal_x = -1; // drag left: reveal the right columns first
        let mut sweep_steps = 0usize;
        loop {
            if self.detect()? != Screen::EventMap {
                return Err("拖图搜索时离开了活动地图".into());
            }
            if let Some(found) = self.refresh_label(text)? {
                return Ok(Some(found));
            }
            if sweep_steps >= MAX_FIND_STEPS {
                return Ok(None);
            }
            sweep_steps += 1;
            let before = self.thumbnail()?;
            MapNavigator::new(&mut *self.touch, self.screen).pan(reveal_x, Axis::X)?;
            sleep(PAN_SETTLE);
            let after = self.thumbnail()?;
            let moved = map::mean_abs_diff(&before, &after);
            if moved.map_or(true, map::is_boundary) {
                // X edge reached: one row down, then sweep back.
                MapNavigator::new(&mut *self.touch, self.screen).pan(-1, Axis::Y)?;
                sleep(PAN_SETTLE);
                reveal_x = -reveal_x;
            }
        }
    }

    fn detect(&mut self) -> Result<Screen, String> {
        self.client
            .detect(None)
            .map_err(|error| format!("画面识别失败: {error}"))
    }

    fn labels(&mut self) -> Result<Vec<MapLabel>, String> {
        let result = self
            .client
            .read_map_labels(None)
            .map_err(|error| format!("区域标签识别失败: {error}"))?;
        Ok(map::usable_labels(&result.labels))
    }

    fn thumbnail(&mut self) -> Result<Vec<u8>, String> {
        let result = self
            .client
            .frame_thumbnail(None, THUMBNAIL.0, THUMBNAIL.1)
            .map_err(|error| format!("缩略图获取失败: {error}"))?;
        if !result.ok || result.pixels.is_empty() {
            return Err("缩略图获取失败".into());
        }
        Ok(result.pixels)
    }

    /// Pinch the map fully out, rewinding after every round. Returns the
    /// number of labels visible when it stopped.
    fn zoom_out(&mut self) -> Result<usize, String> {
        let mut best = self.labels()?.len();
        if best >= TARGET_LABELS {
            self.rewind()?;
            return Ok(self.labels()?.len());
        }
        let mut stall = 0;
        for _ in 0..MAX_ZOOM_ROUNDS {
            MapNavigator::new(&mut *self.touch, self.screen).zoom_out()?;
            sleep(PINCH_SETTLE);
            if self.detect()? != Screen::EventMap {
                break;
            }
            self.rewind()?;
            let now = self.labels()?.len();
            if now >= TARGET_LABELS {
                return Ok(now);
            }
            if now <= best {
                stall += 1;
                if stall >= 2 {
                    break;
                }
            } else {
                stall = 0;
            }
            best = best.max(now);
        }
        Ok(best)
    }

    /// Drag the map to the top-left corner. Boundary detection is pixel
    /// based: the map edges are empty terrain with no labels, so label
    /// stability cannot distinguish "at the edge" from "no new labels".
    fn rewind(&mut self) -> Result<(), String> {
        for axis in [Axis::X, Axis::Y] {
            for _ in 0..MAX_PAN_STEPS {
                let before = self.thumbnail()?;
                MapNavigator::new(&mut *self.touch, self.screen).pan(1, axis)?;
                sleep(PAN_SETTLE);
                let after = self.thumbnail()?;
                if map::mean_abs_diff(&before, &after)
                    .is_some_and(map::is_boundary)
                {
                    break;
                }
                if self.detect()? != Screen::EventMap {
                    return Err("拖动到地图边界时离开了活动地图".into());
                }
            }
        }
        Ok(())
    }

    fn visit_labels(
        &mut self,
        labels: &[MapLabel],
        cards: &mut Vec<QuestCard>,
    ) -> Result<(), String> {
        for label in labels {
            if self.detect()? != Screen::EventMap {
                return Err("扫描过程中离开了活动地图".into());
            }
            // The map may shift after opening/closing a card, so the
            // coordinates captured before the first tap can be stale.
            let Some(fresh) = self.refresh_label(&label.text)? else {
                continue;
            };
            let point = map::label_point(&fresh, self.screen);
            self.touch.tap(point.0, point.1)?;
            sleep(TAP_SETTLE);

            if self.detect()? != Screen::NodeDetail {
                // A region label can open an area-info panel instead of a
                // quest card; close it and move on.
                self.close_card()?;
                continue;
            }
            let card = self.read_card(&fresh.text)?;
            self.close_card()?;
            if let Some(card) = card {
                if card.is_real_quest() {
                    cards.push(card);
                }
            }
        }
        Ok(())
    }

    fn refresh_label(&mut self, text: &str) -> Result<Option<MapLabel>, String> {
        let labels = self.labels()?;
        if let Some(exact) = labels.iter().find(|label| label.text.trim() == text.trim()) {
            return Ok(Some(exact.clone()));
        }
        Ok(labels
            .into_iter()
            .find(|label| map::hint_ok(&label.text, text)))
    }

    /// Cards fade in, so a single unreadable frame is not proof the card
    /// is empty; retry before giving up (reference `read_card`).
    fn read_card(&mut self, label: &str) -> Result<Option<QuestCard>, String> {
        for attempt in 0..CARD_RETRIES {
            let result = self
                .client
                .read_quest_card(None)
                .map_err(|error| format!("任务卡识别失败: {error}"))?;
            if result.found {
                return Ok(Some(QuestCard::from_result(result, label.to_string())));
            }
            if attempt + 1 < CARD_RETRIES {
                sleep(CARD_RETRY_DELAY);
            }
        }
        Ok(None)
    }

    /// Close the card and confirm the map is back. Locked cards may not
    /// have a close button, so an empty-space tap is the fallback.
    fn close_card(&mut self) -> Result<(), String> {
        let (x, y) = normalized_point(CARD_CLOSE_POINT, self.screen);
        self.touch.tap(x, y)?;
        sleep(CLOSE_SETTLE);
        if self.detect()? == Screen::EventMap {
            return Ok(());
        }
        let (x, y) = normalized_point(CARD_CLOSE_FALLBACK, self.screen);
        self.touch.tap(x, y)?;
        sleep(CLOSE_SETTLE);
        if self.detect()? == Screen::EventMap {
            return Ok(());
        }
        Err("关闭任务卡后没有回到活动地图".into())
    }

    /// Wait for the map to render, then report whether it is showing.
    pub(crate) fn wait_for_map(&mut self) -> Result<bool, String> {
        for _ in 0..3 {
            if self.detect()? == Screen::EventMap {
                return Ok(true);
            }
            sleep(MAP_SETTLE);
        }
        Ok(false)
    }
}

fn normalized_point(point: (f64, f64), screen: (u32, u32)) -> (u32, u32) {
    (
        (point.0 * f64::from(screen.0.saturating_sub(1))).round() as u32,
        (point.1 * f64::from(screen.1.saturating_sub(1))).round() as u32,
    )
}

/// Save a scan for the three-hour cache window.
pub(crate) fn save_cache(path: &std::path::Path, cards: Vec<QuestCard>) -> Result<(), String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    ScanCache::new(cards, now).save(path)
}
