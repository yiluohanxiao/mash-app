//! Event-farm runtime (M2): pick a quest on the event map and enter it.
//!
//! The main loop routes `EventMap` / `NodeDetail` / `QuestConfirm` / `Gate`
//! here while the event-farm mode is active. From `SupportSelect` onward the
//! ordinary battle handlers take over untouched; M3 adds the loop back to the
//! map after settlement.
//!
//! Flow per poll:
//! `NeedMap` — scan (or reuse the 3-hour cache), pick the target card, tap
//! its region label → `AwaitingNodeDetail`; `NodeDetail` taps ▶ →
//! `AwaitingBattle`; `QuestConfirm` taps 开始. `Gate` means the card was
//! closed and we drifted into the free-quest list: close it and retry.

use super::cache::{ScanCache, SCAN_CACHE_MAX_AGE};
use super::card::QuestCard;
use super::map;
use super::scan::{save_cache, EventScanner};
use crate::automation_error::AutomationResult;
use crate::runner::{EventFarmConfig, Point, Runner, ACTION_DELAY};
use crate::screen::Screen;
use std::path::PathBuf;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

/// Quest-detail card ▶ (forward) button, reference `nav.NODE_FORWARD`.
const NODE_FORWARD: (f64, f64) = (1370.0 / 1920.0, 320.0 / 1080.0);
/// Quest-confirm dialog 开始 button, reference `nav.QUEST_START`.
const QUEST_START: (f64, f64) = (1259.0 / 1920.0, 846.0 / 1080.0);
/// Quest-detail card 关闭 button, reference `nav.CARD_CLOSE`.
const CARD_CLOSE: (f64, f64) = (163.0 / 1920.0, 63.0 / 1080.0);
/// How often one label may be tapped (or fail to be found) before the run
/// stops instead of silently burning AP on a half-understood map.
const MAX_CARD_ATTEMPTS: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EventFarmPhase {
    /// Waiting on the event map; target not tapped yet.
    NeedMap,
    /// Label tapped, waiting for the quest-detail card.
    AwaitingNodeDetail,
    /// ▶ tapped, waiting for the quest-confirm dialog and battle setup.
    AwaitingBattle,
}

pub(crate) struct EventFarmRuntime {
    pub(crate) config: EventFarmConfig,
    pub(crate) phase: EventFarmPhase,
    /// Card chosen from the scan; `label` is the region label to tap.
    pub(crate) pending_card: Option<QuestCard>,
    /// Taps on the current label, and failed label lookups. Reset when a
    /// new target is picked.
    pub(crate) label_attempts: u32,
}

impl EventFarmRuntime {
    pub(crate) fn new(config: EventFarmConfig) -> Self {
        Self {
            config,
            phase: EventFarmPhase::NeedMap,
            pending_card: None,
            label_attempts: 0,
        }
    }
}

/// Choose the quest to farm. An explicit target matches by name
/// (reference `_match`: containment either way); otherwise the
/// highest-level unlocked farm quest wins. Locked cards never match.
pub(crate) fn pick_target_card(
    cards: &[QuestCard],
    target: Option<&str>,
) -> Result<QuestCard, String> {
    if let Some(target) = target.filter(|t| !t.trim().is_empty()) {
        let matches: Vec<&QuestCard> = cards
            .iter()
            .filter(|card| !card.locked && map::hint_ok(&card.name, target))
            .collect();
        return matches
            .iter()
            .max_by_key(|card| (card.level_key(), card.ap))
            .map(|card| (*card).clone())
            .ok_or_else(|| format!("扫描结果里没有名为「{target}」的可打关卡"));
    }
    super::card::best_farm_quest(cards)
        .cloned()
        .ok_or_else(|| "扫描结果里没有可打的活动关卡（全部锁定或 AP 不足）".into())
}

fn fresh_scan_cache(path: &std::path::Path) -> Option<ScanCache> {
    let cache = ScanCache::load(path)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    cache.is_fresh(now, SCAN_CACHE_MAX_AGE).then_some(cache)
}

impl Runner {
    pub(crate) fn event_farm_active(&self) -> bool {
        self.event_farm_runtime.is_some()
    }

    pub(crate) fn handle_event_screen(&mut self, screen: Screen) -> AutomationResult<()> {
        match screen {
            Screen::EventMap => self.handle_event_map(),
            Screen::NodeDetail => self.handle_node_detail(),
            Screen::QuestConfirm => self.handle_quest_confirm(),
            Screen::Gate => self.handle_event_gate(),
            _ => Ok(()),
        }
    }

    fn handle_event_map(&mut self) -> AutomationResult<()> {
        let phase = self
            .event_farm_runtime
            .as_ref()
            .map(|runtime| runtime.phase)
            .unwrap_or(EventFarmPhase::NeedMap);
        if phase != EventFarmPhase::NeedMap {
            // The card was closed without entering (▶ missed, dialog
            // dismissed). Retry the same label a bounded number of times.
            let attempts = self
                .event_farm_runtime
                .as_mut()
                .map(|runtime| {
                    runtime.label_attempts += 1;
                    runtime.phase = EventFarmPhase::NeedMap;
                    runtime.label_attempts
                })
                .unwrap_or(0);
            if attempts > MAX_CARD_ATTEMPTS {
                self.fail_action(
                    "EventMap",
                    "点开任务卡",
                    "连续多次没能从地图进入任务卡".into(),
                );
                return Ok(());
            }
            self.emit(
                "EventMap",
                &format!("卡片被关闭了，重新点开任务卡 ({attempts}/{MAX_CARD_ATTEMPTS})"),
            );
        }

        if self
            .event_farm_runtime
            .as_ref()
            .is_some_and(|runtime| runtime.pending_card.is_none())
        {
            match self.select_event_target() {
                Ok(card) => {
                    self.emit(
                        "EventMap",
                        &format!(
                            "目标关卡「{}」等级{} AP{}（区域 {}）",
                            card.name,
                            card.level_text(),
                            card.ap,
                            card.label
                        ),
                    );
                    if let Some(runtime) = self.event_farm_runtime.as_mut() {
                        runtime.pending_card = Some(card);
                        runtime.label_attempts = 0;
                    }
                }
                Err(error) => {
                    self.fail_action("EventMap", "选择活动关卡", error);
                    return Ok(());
                }
            }
        }

        let Some(label) = self
            .event_farm_runtime
            .as_ref()
            .and_then(|runtime| runtime.pending_card.as_ref())
            .map(|card| card.label.clone())
        else {
            return Ok(());
        };
        match self.find_event_label_point(&label)? {
            Some(point) => {
                self.emit_debug("EventMap", &format!("点击区域标签「{label}」"));
                if !self.tap_screen_point("EventMap", point) {
                    return Ok(());
                }
                if let Some(runtime) = self.event_farm_runtime.as_mut() {
                    runtime.phase = EventFarmPhase::AwaitingNodeDetail;
                }
                thread::sleep(super::scan::TAP_SETTLE);
            }
            None => {
                // The map shifted or OCR missed this frame; retry on the
                // next poll rather than tapping a guessed coordinate.
                let attempts = self
                    .event_farm_runtime
                    .as_mut()
                    .map(|runtime| {
                        runtime.label_attempts += 1;
                        runtime.label_attempts
                    })
                    .unwrap_or(0);
                if attempts > MAX_CARD_ATTEMPTS {
                    self.fail_action(
                        "EventMap",
                        "定位区域标签",
                        format!("地图上找不到标签「{label}」"),
                    );
                    return Ok(());
                }
                self.emit_warn(
                    "EventMap",
                    &format!("地图上暂未读到标签「{label}」，等待重试 ({attempts}/{MAX_CARD_ATTEMPTS})"),
                );
                thread::sleep(ACTION_DELAY);
            }
        }
        Ok(())
    }

    fn handle_node_detail(&mut self) -> AutomationResult<()> {
        let phase = self
            .event_farm_runtime
            .as_ref()
            .map(|runtime| runtime.phase)
            .unwrap_or(EventFarmPhase::NeedMap);
        if phase != EventFarmPhase::AwaitingNodeDetail {
            self.emit_debug("NodeDetail", "任务卡已打开，但活动流程未在等它；等待下一帧");
            return Ok(());
        }
        self.emit("NodeDetail", "点击 ▶ 进入任务");
        if self.tap_at("NodeDetail", Point::new(NODE_FORWARD.0, NODE_FORWARD.1)) {
            if let Some(runtime) = self.event_farm_runtime.as_mut() {
                runtime.phase = EventFarmPhase::AwaitingBattle;
            }
            thread::sleep(ACTION_DELAY);
        }
        Ok(())
    }

    fn handle_quest_confirm(&mut self) -> AutomationResult<()> {
        self.emit("QuestConfirm", "点击「开始任务」");
        if self.tap_at("QuestConfirm", Point::new(QUEST_START.0, QUEST_START.1)) {
            thread::sleep(ACTION_DELAY);
        }
        Ok(())
    }

    /// The card got closed and we drifted into the free-quest list. Only
    /// the top-left 关闭 is safe here: anything else on that page can
    /// start a quest and spend AP.
    fn handle_event_gate(&mut self) -> AutomationResult<()> {
        let phase = self
            .event_farm_runtime
            .as_ref()
            .map(|runtime| runtime.phase)
            .unwrap_or(EventFarmPhase::NeedMap);
        if phase == EventFarmPhase::NeedMap {
            self.emit_debug("Gate", "活动流程未在进关途中，等待下一帧");
            return Ok(());
        }
        self.emit("Gate", "误入自由本列表，点击关闭退出");
        if self.tap_at("Gate", Point::new(CARD_CLOSE.0, CARD_CLOSE.1)) {
            if let Some(runtime) = self.event_farm_runtime.as_mut() {
                runtime.phase = EventFarmPhase::NeedMap;
            }
            thread::sleep(ACTION_DELAY);
        }
        Ok(())
    }

    /// Cards from the 3-hour scan cache, or a live scan (saved back).
    fn event_scan_cards(&mut self) -> Result<Vec<QuestCard>, String> {
        let path = self.scan_cache_path();
        if let Some(cache) = path.as_deref().and_then(fresh_scan_cache) {
            self.emit_debug("EventMap", "使用 3 小时内的扫图缓存");
            return Ok(cache.cards);
        }
        self.emit("EventMap", "扫描活动地图（约 1–3 分钟）…");
        let client = self.sidecar.as_mut().expect("runner sidecar missing");
        let touch = &mut *self.touch;
        let mut scanner = EventScanner::new(client, touch, (self.screen_w, self.screen_h));
        let outcome = scanner.scan()?;
        if let Some(path) = &path {
            save_cache(path, outcome.cards.clone())
                .map_err(|error| format!("写入扫图缓存失败: {error}"))?;
        }
        Ok(outcome.cards)
    }

    fn select_event_target(&mut self) -> Result<QuestCard, String> {
        let target = self
            .event_farm_runtime
            .as_ref()
            .and_then(|runtime| runtime.config.target_quest.clone());
        let cards = self.event_scan_cards()?;
        pick_target_card(&cards, target.as_deref())
    }

    fn find_event_label_point(&mut self, text: &str) -> Result<Option<(u32, u32)>, String> {
        let result = self
            .sidecar()
            .read_map_labels(None)
            .map_err(|error| format!("区域标签识别失败: {error}"))?;
        let labels = map::usable_labels(&result.labels);
        let found = labels
            .iter()
            .find(|label| label.text.trim() == text.trim())
            .or_else(|| {
                labels
                    .iter()
                    .find(|label| map::hint_ok(&label.text, text))
            });
        Ok(found.map(|label| map::label_point(label, (self.screen_w, self.screen_h))))
    }

    fn scan_cache_path(&self) -> Option<PathBuf> {
        use tauri::Manager;
        self.app_handle
            .path()
            .app_data_dir()
            .ok()
            .map(|dir| dir.join("event").join("scout.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(name: &str, level: u32, plus: u32, ap: u32, locked: bool, label: &str) -> QuestCard {
        QuestCard {
            name: name.into(),
            level,
            plus,
            ap,
            locked,
            cond: String::new(),
            label: label.into(),
        }
    }

    #[test]
    fn explicit_target_matches_by_name_and_prefers_the_highest_level() {
        let cards = [
            card("残骸小镇", 90, 2, 40, false, "小镇"),
            card("技能升级本", 90, 0, 40, false, "小镇"),
        ];
        let picked = pick_target_card(&cards, Some("残骸小镇")).unwrap();
        assert_eq!(picked.plus, 2);
    }

    #[test]
    fn explicit_target_never_picks_a_locked_card() {
        let cards = [card("残骸小镇", 90, 2, 40, true, "小镇")];
        assert!(pick_target_card(&cards, Some("残骸小镇")).is_err());
    }

    #[test]
    fn empty_target_falls_back_to_the_best_farm_quest() {
        let cards = [
            card("剧情", 10, 0, 5, false, "a"),
            card("刷点本", 90, 1, 40, false, "b"),
            card("高难度", 90, 2, 40, false, "c"),
        ];
        let picked = pick_target_card(&cards, Some("  ")).unwrap();
        assert_eq!(picked.name, "刷点本");
        assert_eq!(picked.plus, 1);
    }

    #[test]
    fn no_farmable_card_is_an_error_not_a_story_quest() {
        let cards = [card("序幕", 10, 0, 0, false, "a")];
        let error = pick_target_card(&cards, None).unwrap_err();
        assert!(error.contains("没有可打的活动关卡"));
    }

    #[test]
    fn enter_points_match_the_reference_1920x1080_coordinates() {
        let to_px = |p: (f64, f64)| {
            (
                (p.0 * 1920.0).round() as u32,
                (p.1 * 1080.0).round() as u32,
            )
        };
        assert_eq!(to_px(NODE_FORWARD), (1370, 320));
        assert_eq!(to_px(QUEST_START), (1259, 846));
        assert_eq!(to_px(CARD_CLOSE), (163, 63));
    }

    #[test]
    fn runtime_starts_waiting_on_the_map_without_a_target() {
        let runtime = EventFarmRuntime::new(EventFarmConfig::default());
        assert_eq!(runtime.phase, EventFarmPhase::NeedMap);
        assert!(runtime.pending_card.is_none());
        assert_eq!(runtime.label_attempts, 0);
    }
}
