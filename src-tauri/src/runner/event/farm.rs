//! Event-farm runtime: loop quests on the event map and recover from the
//! event-only dialogs in between.
//!
//! The main loop routes `EventMap` / `NodeDetail` / `QuestConfirm` / `Gate`
//! plus the event-only dialog screens here while the event-farm mode is
//! active. From `SupportSelect` onward the ordinary battle handlers take
//! over untouched; the settlement page closes back to the map and the next
//! `EventMap` frame picks the following quest (delta-rescanning, since
//! story progress may have unlocked cards).
//!
//! Flow per poll:
//! `NeedMap` — scan (or reuse the 3-hour cache; delta-rescan after a
//! settlement), pick the target card, tap its region label →
//! `AwaitingNodeDetail`; `NodeDetail` taps ▶ → `AwaitingBattle`;
//! `QuestConfirm` taps 开始 and clears the failure streak. `Gate` means the
//! card was closed and we drifted into the free-quest list: close it and
//! retry. Repeated entry failures blacklist the label (`BAD_LIMIT`) and
//! back off (5/10/20/40/60s) before picking another card.

use super::cache::{ScanCache, SCAN_CACHE_MAX_AGE};
use super::card::{
    best_farm_quest, excluded_label, plan_once, EventPlan, QuestCard, BAD_LIMIT,
};
use super::map;
use super::scan::{save_cache, EventScanner};
use crate::automation_error::{AutomationResult, AutomationResultExt};
use crate::runner::{EventFarmConfig, Point, Runner, ACTION_DELAY};
use crate::screen::{NormRect, Screen};
use std::collections::HashMap;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Quest-detail card ▶ (forward) button, reference `nav.NODE_FORWARD`.
pub(crate) const NODE_FORWARD: (f64, f64) = (1370.0 / 1920.0, 320.0 / 1080.0);
/// Quest-confirm dialog 开始 button, reference `nav.QUEST_START`.
pub(crate) const QUEST_START_POINT: (f64, f64) = (1259.0 / 1920.0, 846.0 / 1080.0);
/// Quest-detail card 关闭 button, reference `nav.CARD_CLOSE`.
const CARD_CLOSE: (f64, f64) = (163.0 / 1920.0, 63.0 / 1080.0);
/// Story screens: 右上「跳过」, then the confirm dialog's 「是」
/// (reference `nav.SKIP` / `nav.SKIP_YES`).
const SKIP_BUTTON: (f64, f64) = (1786.0 / 1920.0, 60.0 / 1080.0);
const SKIP_CONFIRM_YES: (f64, f64) = (1241.0 / 1920.0, 840.0 / 1080.0);
/// Generic "tap to continue" point that avoids bottom toasts
/// (reference `nav.TAP_ANYWHERE`).
const TAP_ANYWHERE: (f64, f64) = (960.0 / 1920.0, 400.0 / 1080.0);
/// Tip dialog 「退出」, center of the reference tap box
/// `_common` tip: (700, 790, 1250, 900).
const TIP_QUIT: (f64, f64) = (975.0 / 1920.0, 845.0 / 1080.0);
/// List-update dialog 「否」, reference `_common` fixed fallback (592, 755).
const LIST_UPDATE_NO: (f64, f64) = (592.0 / 1920.0, 755.0 / 1080.0);
/// Repeat-quest dialog 「关闭」, center of the reference tap box
/// `_common` repeat_confirm: (500, 820, 900, 930).
const REPEAT_CONFIRM_CLOSE: (f64, f64) = (700.0 / 1920.0, 875.0 / 1080.0);
/// Servant-full dialog 「关闭」 — never 变还 / 强化 / 灵基保管室
/// (reference `nav.SERVANT_FULL_CLOSE`).
const SERVANT_FULL_CLOSE: (f64, f64) = (860.0 / 1920.0, 720.0 / 1080.0);
/// Reward-notice confirmation (宝具强化专用灵基) 「关闭」, measured on the
/// Hallowe'en 2026 CN dialog (button centered at (960, 850)).
const NP_CE_NOTICE_CLOSE: (f64, f64) = (960.0 / 1920.0, 850.0 / 1080.0);
/// 游戏公告 dialog top-right ✕ (client-pushed announcement, can cover any
/// screen mid-run; the ✕ is the dialog's only control).
const GAME_NOTICE_CLOSE: (f64, f64) = (1865.0 / 1920.0, 45.0 / 1080.0);
/// Template + search region for the same button (element `close_button`
/// in the CN config); the fixed coordinate above stays as the fallback.
const NP_CE_NOTICE_CLOSE_TEMPLATE: &str = "button_np_ce_notice_close";
const NP_CE_NOTICE_CLOSE_REGION: NormRect = NormRect {
    x: 0.32,
    y: 0.70,
    w: 0.36,
    h: 0.16,
};
/// How often one label may be tapped (or fail to be found) before the run
/// gives up on that card instead of silently burning AP on a half-understood
/// map.
const MAX_CARD_ATTEMPTS: u32 = 3;
/// Minimum spacing between「关闭」taps on an unexpected card: once the card
/// closes, this coordinate is the event map's back arrow, so a rushed
/// second tap leaves the event for the home screen.
const NODE_CLOSE_MIN_INTERVAL: Duration = Duration::from_secs(3);
/// A card that survives this many spaced close taps stops the run.
const NODE_CLOSE_MAX: u32 = 5;
/// `PartyConfirm` is never auto-confirmed; this many consecutive
/// observations instead stop the run so the user can confirm the party.
const PARTY_CONFIRM_MAX_OBSERVATIONS: u32 = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EventFarmPhase {
    /// Waiting on the event map; target not tapped yet.
    NeedMap,
    /// Label tapped, waiting for the quest-detail card.
    AwaitingNodeDetail,
    /// ▶ tapped, waiting for the quest-confirm dialog and battle setup.
    AwaitingBattle,
    /// Settlement closed; waiting for the game to redraw the event map.
    /// The next `EventMap` observation does a delta rescan because story
    /// progress may have unlocked cards.
    AwaitingReturnToMap,
}

pub(crate) struct EventFarmRuntime {
    pub(crate) config: EventFarmConfig,
    pub(crate) phase: EventFarmPhase,
    /// Card chosen from the scan; `label` is the region label to tap.
    pub(crate) pending_card: Option<QuestCard>,
    /// Taps on the current label, and failed label lookups. Reset when a
    /// new target is picked.
    pub(crate) label_attempts: u32,
    /// Settlement just completed: the next target pick must delta-rescan
    /// (points moved, story may have unlocked).
    pub(crate) rescan_needed: bool,
    /// Consecutive quest-entry failures, driving the retry backoff.
    pub(crate) failures: u32,
    /// Region labels whose quest repeatedly failed to open this run.
    /// In-memory only, cleared when automation restarts.
    pub(crate) bad_labels: HashMap<String, u32>,
    /// Consecutive `PartyConfirm` observations; the dialog is never
    /// auto-confirmed, so a stuck one stops the run.
    pub(crate) party_confirm_seen: u32,
    /// Cards behind the current target pick (scan cache or live scan).
    /// The ▶-tap verification merges a live card read into this list.
    pub(crate) last_cards: Vec<QuestCard>,
    /// A scan saw zero locked cards: every visible quest is already
    /// unlocked, so settlement no longer needs the delta rescan. The run
    /// keeps farming the highest-level quest; cards that unlock behind
    /// new sections later would be missed until a restart re-scans.
    pub(crate) unlock_progress_complete: bool,
    /// Last「关闭」tap on an unexpected card, and how many were issued.
    /// The card's 关闭 sits where the event map's back arrow lives, so a
    /// second tap fired before the card finishes closing would leave the
    /// event for the home screen.
    pub(crate) last_node_close: Option<std::time::Instant>,
    pub(crate) node_close_taps: u32,
}

impl EventFarmRuntime {
    pub(crate) fn new(config: EventFarmConfig) -> Self {
        Self {
            config,
            phase: EventFarmPhase::NeedMap,
            pending_card: None,
            label_attempts: 0,
            rescan_needed: false,
            failures: 0,
            bad_labels: HashMap::new(),
            party_confirm_seen: 0,
            last_cards: Vec::new(),
            last_node_close: None,
            node_close_taps: 0,
            unlock_progress_complete: false,
        }
    }
}

/// Choose the quest to farm. An explicit target matches by name
/// (reference `_match`: containment either way); otherwise the
/// highest-level unlocked farm quest wins. Locked cards never match, and
/// labels with `BAD_LIMIT` consecutive failures are skipped.
pub(crate) fn pick_target_card(
    cards: &[QuestCard],
    target: Option<&str>,
    bad_labels: &HashMap<String, u32>,
) -> Result<QuestCard, String> {
    if let Some(target) = target.filter(|t| !t.trim().is_empty()) {
        let matches: Vec<&QuestCard> = cards
            .iter()
            .filter(|card| {
                !card.locked && !excluded_label(bad_labels, &card.label) && map::hint_ok(&card.name, target)
            })
            .collect();
        return matches
            .iter()
            .max_by_key(|card| (card.level_key(), card.ap))
            .map(|card| (*card).clone())
            .ok_or_else(|| format!("扫描结果里没有名为「{target}」的可打关卡"));
    }
    let candidates: Vec<QuestCard> = cards
        .iter()
        .filter(|card| !excluded_label(bad_labels, &card.label))
        .cloned()
        .collect();
    best_farm_quest(&candidates)
        .cloned()
        .ok_or_else(|| "扫描结果里没有可打的活动关卡（全部锁定、拉黑或 AP 不足）".into())
}

/// Whether the card just read from the open quest-detail box differs from
/// the cached card the planner picked. Only identity fields count — name,
/// AP and recommended level. `cond` and `label` are bookkeeping, and a
/// `locked` difference is resolved by re-deciding on the merged list.
pub(crate) fn cards_differ(cached: &QuestCard, live: &QuestCard) -> bool {
    cached.name.trim() != live.name.trim()
        || cached.ap != live.ap
        || cached.level != live.level
        || cached.plus != live.plus
}

/// Replace the cached card opened by `live`'s region label with the live
/// read (append when the cache has no entry for that label).
pub(crate) fn merge_card(mut cards: Vec<QuestCard>, live: QuestCard) -> Vec<QuestCard> {
    if let Some(slot) = cards.iter_mut().find(|card| card.label == live.label) {
        *slot = live;
    } else {
        cards.push(live);
    }
    cards
}

/// Decide the quest to enter from `cards` — the selector's core, shared by
/// the map pick (`select_event_target`) and the live-card verification.
pub(crate) fn decide_target(
    cards: &[QuestCard],
    target: Option<&str>,
    points: Option<u32>,
    bad_labels: &HashMap<String, u32>,
) -> Result<QuestCard, String> {
    match target.filter(|t| !t.trim().is_empty()) {
        Some(target) => pick_target_card(cards, Some(target), bad_labels),
        None => match plan_once(cards, points, bad_labels)? {
            EventPlan::Story(card) | EventPlan::Farm(card) => Ok(card),
        },
    }
}

/// Whether a scan saw no locked cards at all — every visible quest is
/// already unlocked, so nothing a delta rescan looks for remains and the
/// post-settlement rescan can be skipped for the rest of the run. An
/// empty scan never qualifies: it means the scan itself failed, not
/// that everything is unlocked.
pub(crate) fn scan_shows_nothing_locked(cards: &[QuestCard]) -> bool {
    !cards.is_empty() && cards.iter().all(|card| !card.locked)
}

/// Outcome of verifying the pending card against a live read before
/// tapping ▶.
pub(crate) enum VerifyOutcome {
    /// The card matches (or could not be read): tap ▶.
    Proceed,
    /// The card is not what the scan cached: close it and pick again.
    Reselect,
}

/// Sleep before the next retry after `failures` consecutive quest-entry
/// failures (reference `flow.backoff_seconds`).
pub(crate) fn backoff_seconds(failures: u32) -> u64 {
    match failures {
        0 => 0,
        1..=5 => (5u64 << (failures - 1)).min(60),
        _ => 60,
    }
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
        if !self.event_farm_active() {
            // Battle runs never route here (every one of these screens is
            // event-only detection), but stay passive if one ever arrives.
            return Ok(());
        }
        match screen {
            Screen::EventMap => self.handle_event_map(),
            Screen::NodeDetail => self.handle_node_detail(),
            Screen::QuestConfirm => self.handle_quest_confirm(),
            Screen::Gate => self.handle_event_gate(),
            Screen::Story => self.handle_story_skip(),
            Screen::SkipConfirm => self.handle_skip_confirm(),
            Screen::TapContinue => self.handle_tap_continue(),
            Screen::Tip => self.handle_tip_quit(),
            Screen::ListUpdate => self.handle_list_update(),
            Screen::NpCeNotice => self.handle_np_ce_notice(),
            Screen::GameNotice => self.handle_game_notice(),
            Screen::RepeatConfirm => self.handle_repeat_confirm(),
            Screen::ServantFull => self.handle_servant_full(),
            Screen::Defeat => self.handle_defeat(),
            Screen::PartyConfirm => self.handle_party_confirm(),
            _ => Ok(()),
        }
    }

    /// Never auto-confirm a party change; a dialog that will not go away
    /// stops the run for a manual decision instead of tapping blind.
    fn handle_party_confirm(&mut self) -> AutomationResult<()> {
        let Some(seen) = self.event_farm_runtime.as_mut().map(|runtime| {
            runtime.party_confirm_seen += 1;
            runtime.party_confirm_seen
        }) else {
            return Ok(());
        };
        if seen > PARTY_CONFIRM_MAX_OBSERVATIONS {
            self.fail_action(
                "PartyConfirm",
                "编队确认处理",
                "编队确认弹窗长时间未消失：不自动确认编队，已停止，请人工确认队伍".into(),
            );
            return Ok(());
        }
        self.emit_debug(
            "PartyConfirm",
            "编队确认弹窗，等待人工确认；不自动点击，避免意外编队",
        );
        Ok(())
    }

    /// A story scene opened mid-run (main-quest unlocks do this). Skip it:
    /// 右上「跳过」, then the confirm dialog's 「是」.
    fn handle_story_skip(&mut self) -> AutomationResult<()> {
        self.emit("Story", "点击「跳过」跳过活动剧情");
        if self.tap_at("Story", Point::new(SKIP_BUTTON.0, SKIP_BUTTON.1)) {
            thread::sleep(ACTION_DELAY);
        }
        Ok(())
    }

    fn handle_skip_confirm(&mut self) -> AutomationResult<()> {
        self.emit("SkipConfirm", "确认跳过剧情");
        if self.tap_at("SkipConfirm", Point::new(SKIP_CONFIRM_YES.0, SKIP_CONFIRM_YES.1)) {
            thread::sleep(ACTION_DELAY);
        }
        Ok(())
    }

    /// Post-scene "tap to continue" overlay.
    fn handle_tap_continue(&mut self) -> AutomationResult<()> {
        self.emit("TapContinue", "点击继续");
        if self.tap_at("TapContinue", Point::new(TAP_ANYWHERE.0, TAP_ANYWHERE.1)) {
            thread::sleep(ACTION_DELAY);
        }
        Ok(())
    }

    /// Tip dialog (e.g. 圣晶石不足 notices). Only 退出 is safe: the other
    /// buttons lead to purchase/recharge surfaces.
    fn handle_tip_quit(&mut self) -> AutomationResult<()> {
        self.emit("Tip", "提示弹窗，点击「退出」");
        if self.tap_at("Tip", Point::new(TIP_QUIT.0, TIP_QUIT.1)) {
            thread::sleep(ACTION_DELAY);
        }
        Ok(())
    }

    /// Support-list update prompt. Refreshing costs nothing but stalls the
    /// loop, so answer 否.
    fn handle_list_update(&mut self) -> AutomationResult<()> {
        self.emit("ListUpdate", "列表更新弹窗，点击「否」");
        if self.tap_at("ListUpdate", Point::new(LIST_UPDATE_NO.0, LIST_UPDATE_NO.1)) {
            thread::sleep(ACTION_DELAY);
        }
        Ok(())
    }

    /// Reward-notice confirmation (～关于宝具强化专用灵基～). The dialog has
    /// no other interactive element, so 关闭 is the only safe tap. The
    /// button is located by template; the fixed measured coordinate is the
    /// fallback when the template cannot be matched.
    fn handle_np_ce_notice(&mut self) -> AutomationResult<()> {
        self.emit("NpCeNotice", "宝具强化专用灵基说明弹窗，点击「关闭」");
        let located = self
            .sidecar()
            .find_element(
                None,
                NP_CE_NOTICE_CLOSE_TEMPLATE,
                NP_CE_NOTICE_CLOSE_REGION,
                0.8,
            )
            .recoverable()?
            .ok()
            .flatten();
        let point = located
            .unwrap_or(Point::new(NP_CE_NOTICE_CLOSE.0, NP_CE_NOTICE_CLOSE.1));
        if self.tap_at("NpCeNotice", point) {
            thread::sleep(ACTION_DELAY);
        }
        Ok(())
    }

    /// 游戏公告 dialog: the client pushes announcements on its own
    /// schedule, so it can cover the map or a transition mid-run. The ✕
    /// is the only control; closing reveals whatever was underneath.
    fn handle_game_notice(&mut self) -> AutomationResult<()> {
        self.emit("GameNotice", "游戏公告弹窗，点击右上 ✕ 关闭");
        if self.tap_at("GameNotice", Point::new(GAME_NOTICE_CLOSE.0, GAME_NOTICE_CLOSE.1)) {
            thread::sleep(ACTION_DELAY);
        }
        Ok(())
    }

    /// Repeat-quest prompt. 关闭, never 再来一次: the strategy picks the
    /// next quest itself (points may have unlocked story).
    fn handle_repeat_confirm(&mut self) -> AutomationResult<()> {
        self.emit("RepeatConfirm", "连战确认弹窗，点击「关闭」");
        if self.tap_at(
            "RepeatConfirm",
            Point::new(REPEAT_CONFIRM_CLOSE.0, REPEAT_CONFIRM_CLOSE.1),
        ) {
            if let Some(runtime) = self.event_farm_runtime.as_mut() {
                runtime.phase = EventFarmPhase::NeedMap;
            }
            thread::sleep(ACTION_DELAY);
        }
        Ok(())
    }

    /// Servant inventory full: close the dialog, then stop. Never 变还 or
    /// 灵基保管室 — deleting servants is the user's call.
    fn handle_servant_full(&mut self) -> AutomationResult<()> {
        self.emit_warn("ServantFull", "从者栏已满：关闭弹窗后停止，请手动清理从者");
        if self.tap_at("ServantFull", Point::new(SERVANT_FULL_CLOSE.0, SERVANT_FULL_CLOSE.1)) {
            thread::sleep(ACTION_DELAY);
        }
        self.fail_action(
            "ServantFull",
            "从者栏满处理",
            "从者栏已满，已停止；请手动清理从者后重新启动".into(),
        );
        Ok(())
    }

    /// Defeat: stop without reviving. 圣晶石复活 is never an option.
    fn handle_defeat(&mut self) -> AutomationResult<()> {
        self.fail_action(
            "Defeat",
            "战斗失败处理",
            "战斗败北：不自动复活，已停止等待人工处理".into(),
        );
        Ok(())
    }

    fn handle_event_map(&mut self) -> AutomationResult<()> {
        // Back on the map: any previous unexpected-card close episode is
        // over, so its tap budget starts fresh.
        if let Some(runtime) = self.event_farm_runtime.as_mut() {
            runtime.last_node_close = None;
            runtime.node_close_taps = 0;
        }
        let phase = self
            .event_farm_runtime
            .as_ref()
            .map(|runtime| runtime.phase)
            .unwrap_or(EventFarmPhase::NeedMap);
        if phase == EventFarmPhase::AwaitingReturnToMap {
            // Back from settlement; the next target pick delta-rescans
            // because story progress may have unlocked cards.
            if let Some(runtime) = self.event_farm_runtime.as_mut() {
                runtime.phase = EventFarmPhase::NeedMap;
                runtime.rescan_needed = true;
                runtime.pending_card = None;
            }
        } else if phase != EventFarmPhase::NeedMap {
            // The card was closed without entering (▶ missed, dialog
            // dismissed). Retry the same label a bounded number of times,
            // then blacklist it and pick a different card after a backoff.
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
                self.blacklist_pending_label("EventMap", "连续多次没能从地图进入任务卡");
                self.apply_backoff("EventMap");
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
                    self.blacklist_pending_label("EventMap", &format!("地图上找不到标签「{label}」"));
                    self.apply_backoff("EventMap");
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
        if phase == EventFarmPhase::AwaitingNodeDetail {
            // Verify the open card against the live read before spending
            // AP on what a stale scan cache picked.
            match self.verify_pending_card() {
                Ok(VerifyOutcome::Proceed) => {}
                Ok(VerifyOutcome::Reselect) => {
                    self.emit("NodeDetail", "关闭任务卡，回到地图重新选本");
                    if let Some(runtime) = self.event_farm_runtime.as_mut() {
                        runtime.phase = EventFarmPhase::NeedMap;
                        runtime.pending_card = None;
                    }
                    if self.tap_at("NodeDetail", Point::new(CARD_CLOSE.0, CARD_CLOSE.1)) {
                        thread::sleep(ACTION_DELAY);
                    }
                    return Ok(());
                }
                Err(error) => {
                    self.emit_warn("NodeDetail", &format!("{error}，按缓存继续"));
                }
            }
            self.emit("NodeDetail", "点击 ▶ 进入任务");
            if self.tap_at("NodeDetail", Point::new(NODE_FORWARD.0, NODE_FORWARD.1)) {
                if let Some(runtime) = self.event_farm_runtime.as_mut() {
                    runtime.phase = EventFarmPhase::AwaitingBattle;
                }
                thread::sleep(ACTION_DELAY);
            }
            return Ok(());
        }
        if phase == EventFarmPhase::AwaitingBattle {
            // ▶ was tapped but the card is still open: the tap missed or
            // this card offers no ▶ at all — the completed last-run card
            // replaces the button with the mission-progress box. Retry a
            // bounded number of times, then blacklist the label, close
            // the card and pick a different quest.
            let attempts = self
                .event_farm_runtime
                .as_mut()
                .map(|runtime| {
                    runtime.label_attempts += 1;
                    runtime.label_attempts
                })
                .unwrap_or(0);
            if attempts > MAX_CARD_ATTEMPTS {
                self.blacklist_pending_label("NodeDetail", "多次点击 ▶ 仍未进入任务");
                self.apply_backoff("NodeDetail");
                if let Some(runtime) = self.event_farm_runtime.as_mut() {
                    runtime.phase = EventFarmPhase::NeedMap;
                }
                self.tap_at("NodeDetail", Point::new(CARD_CLOSE.0, CARD_CLOSE.1));
                thread::sleep(ACTION_DELAY);
                return Ok(());
            }
            self.emit_warn(
                "NodeDetail",
                &format!("点击 ▶ 后仍停在任务卡，重试 ({attempts}/{MAX_CARD_ATTEMPTS})"),
            );
            if self.tap_at("NodeDetail", Point::new(NODE_FORWARD.0, NODE_FORWARD.1)) {
                thread::sleep(ACTION_DELAY);
            }
            return Ok(());
        }
        // The card is open while the flow wants the map — a leftover from
        // an earlier state or a missed close tap. Tap 关闭, but keep the
        // taps spaced: after the card closes this same coordinate is the
        // event map's back arrow, and a rushed second tap exits the event.
        let (spaced, taps) = self
            .event_farm_runtime
            .as_mut()
            .map(|runtime| {
                let spaced = runtime
                    .last_node_close
                    .map(|at| at.elapsed() >= NODE_CLOSE_MIN_INTERVAL)
                    .unwrap_or(true);
                if spaced {
                    runtime.last_node_close = Some(std::time::Instant::now());
                    runtime.node_close_taps += 1;
                }
                (spaced, runtime.node_close_taps)
            })
            .unwrap_or((false, 0));
        if taps > NODE_CLOSE_MAX {
            self.fail_action(
                "NodeDetail",
                "关闭任务卡",
                format!("任务卡连续 {taps} 次「关闭」未消失，已停止，请人工查看").into(),
            );
            return Ok(());
        }
        if !spaced {
            // The previous close tap may still be animating.
            thread::sleep(ACTION_DELAY);
            return Ok(());
        }
        self.emit_debug("NodeDetail", "任务卡未在等待中，点击「关闭」回到地图");
        if self.tap_at("NodeDetail", Point::new(CARD_CLOSE.0, CARD_CLOSE.1)) {
            thread::sleep(super::scan::TAP_SETTLE);
        }
        Ok(())
    }

    fn handle_quest_confirm(&mut self) -> AutomationResult<()> {
        self.emit("QuestConfirm", "点击「开始任务」");
        if self.tap_at(
            "QuestConfirm",
            Point::new(QUEST_START_POINT.0, QUEST_START_POINT.1),
        ) {
            // A confirmed quest start ends the failure streak: backoff only
            // measures consecutive *entry* failures.
            if let Some(runtime) = self.event_farm_runtime.as_mut() {
                runtime.failures = 0;
            }
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

    /// Record a failed entry attempt for the pending card's label, drop the
    /// card, and let the next poll pick a different one. A label failing
    /// `BAD_LIMIT` times is excluded by `pick_target_card` for the rest of
    /// the run (in-memory only).
    fn blacklist_pending_label(&mut self, screen: &str, reason: &str) {
        let outcome = self.event_farm_runtime.as_mut().and_then(|runtime| {
            runtime.pending_card.take().map(|card| {
                let count = runtime.bad_labels.entry(card.label.clone()).or_insert(0);
                *count += 1;
                (card.label, *count)
            })
        });
        self.emit_warn(screen, &format!("{reason}，换一个关卡重试"));
        if let Some((label, count)) = outcome {
            if count >= BAD_LIMIT {
                self.emit_warn(screen, &format!("标签「{label}」已拉黑，本轮跳过"));
            } else {
                self.emit_warn(
                    screen,
                    &format!("标签「{label}」失败计数 {count}/{BAD_LIMIT}"),
                );
            }
        }
    }

    /// Pause after a consecutive quest-entry failure (5/10/20/40/60s cap).
    fn apply_backoff(&mut self, screen: &str) {
        let seconds = self
            .event_farm_runtime
            .as_mut()
            .map(|runtime| {
                runtime.failures += 1;
                runtime.label_attempts = 0;
                backoff_seconds(runtime.failures)
            })
            .unwrap_or(0);
        self.emit_warn(screen, &format!("连续失败，等待 {seconds} 秒后重试"));
        thread::sleep(Duration::from_secs(seconds));
    }

    /// Cards from the 3-hour scan cache, or a live scan (saved back). After
    /// a settlement, `rescan` limits the live scan to newly visible /
    /// newly unlocked labels and merges with the cached list. A scan that
    /// fails because a quest card is open over the map gets one recovery
    /// attempt (see `close_open_quest_card`) before the error propagates.
    fn event_scan_cards(&mut self, rescan: bool) -> Result<Vec<QuestCard>, String> {
        match self.event_scan_cards_inner(rescan) {
            Ok(cards) => Ok(cards),
            Err(error) => {
                self.emit_warn("EventMap", &format!("扫描失败: {error}"));
                if !self.close_open_quest_card() {
                    return Err(error);
                }
                self.emit("EventMap", "已关闭遮挡的任务卡，重试扫描");
                self.event_scan_cards_inner(rescan)
            }
        }
    }

    fn event_scan_cards_inner(&mut self, rescan: bool) -> Result<Vec<QuestCard>, String> {
        let path = self.scan_cache_path();
        let cached = path.as_deref().and_then(fresh_scan_cache);
        if !rescan {
            if let Some(cache) = cached {
                self.emit_debug("EventMap", "使用 3 小时内的扫图缓存");
                return Ok(cache.cards);
            }
            return self.full_event_scan();
        }
        let Some(previous) = cached.map(|cache| cache.cards) else {
            self.emit_warn("EventMap", "没有可用的扫图缓存，回退全量扫描");
            return self.full_event_scan();
        };
        self.emit("EventMap", "增量重扫活动地图（检查新解锁关卡）…");
        let client = self.sidecar.as_mut().expect("runner sidecar missing");
        let touch = &mut *self.touch;
        let mut scanner = EventScanner::new(client, touch, (self.screen_w, self.screen_h));
        match scanner.scan_delta(&previous) {
            Ok(cards) => {
                if let Some(path) = &path {
                    save_cache(path, cards.clone())
                        .map_err(|error| format!("写入扫图缓存失败: {error}"))?;
                }
                Ok(cards)
            }
            Err(error) => {
                self.emit_warn("EventMap", &format!("增量重扫失败，回退全量扫描: {error}"));
                self.full_event_scan()
            }
        }
    }

    fn full_event_scan(&mut self) -> Result<Vec<QuestCard>, String> {
        self.emit("EventMap", "扫描活动地图（约 1–3 分钟）…");
        let client = self.sidecar.as_mut().expect("runner sidecar missing");
        let touch = &mut *self.touch;
        let mut scanner = EventScanner::new(client, touch, (self.screen_w, self.screen_h));
        let outcome = scanner.scan()?;
        if let Some(path) = self.scan_cache_path() {
            save_cache(&path, outcome.cards.clone())
                .map_err(|error| format!("写入扫图缓存失败: {error}"))?;
        }
        Ok(outcome.cards)
    }

    /// Close a quest-detail card that is open over the map (the client
    /// re-opens the last-run card after a settlement, or a scan drag
    /// landed on a node) and wait for the map to come back. Every scan
    /// pre-check fails while the card is up, so `event_scan_cards` calls
    /// this once before giving up. Returns true when the card is gone —
    /// whether the map is behind it or not; the scan's own pre-check
    /// reports a non-map screen with its usual error.
    fn close_open_quest_card(&mut self) -> bool {
        let screen = match self.sidecar().detect(None) {
            Ok(screen) => screen,
            Err(_) => return false,
        };
        if screen != Screen::NodeDetail {
            return false;
        }
        self.emit_warn("NodeDetail", "扫描时任务卡仍开着，点击「关闭」后重试");
        for _ in 0..NODE_CLOSE_MAX {
            if !self.tap_at("NodeDetail", Point::new(CARD_CLOSE.0, CARD_CLOSE.1)) {
                return false;
            }
            // Same guard as the leftover-card branch: after the card
            // closes, this coordinate is the map's back arrow.
            thread::sleep(super::scan::TAP_SETTLE);
            thread::sleep(NODE_CLOSE_MIN_INTERVAL);
            match self.sidecar().detect(None) {
                Ok(Screen::NodeDetail) => continue,
                Ok(_) => return true,
                Err(_) => return false,
            }
        }
        false
    }

    fn select_event_target(&mut self) -> Result<QuestCard, String> {
        let (target, rescan_needed, bad_labels) = self
            .event_farm_runtime
            .as_ref()
            .map(|runtime| {
                (
                    runtime.config.target_quest.clone(),
                    runtime.rescan_needed,
                    runtime.bad_labels.clone(),
                )
            })
            .unwrap_or((None, false, HashMap::new()));
        let unlock_complete = self
            .event_farm_runtime
            .as_ref()
            .is_some_and(|runtime| runtime.unlock_progress_complete);
        let rescan = rescan_needed && !unlock_complete;
        if rescan_needed && unlock_complete {
            self.emit("EventMap", "所有可见关卡均已解锁，跳过增量重扫");
        }
        let cards = self.event_scan_cards(rescan)?;
        if let Some(runtime) = self.event_farm_runtime.as_mut() {
            runtime.last_cards = cards.clone();
            if scan_shows_nothing_locked(&cards) && !runtime.unlock_progress_complete {
                runtime.unlock_progress_complete = true;
                self.emit("EventMap", "扫描未发现待解锁关卡，后续跳过增量重扫");
            }
        }
        let explicit = target.as_deref().filter(|t| !t.trim().is_empty());
        // Points only matter for the automatic plan; skip the OCR read
        // when an explicit target makes the plan irrelevant.
        let points = if explicit.is_none() {
            self.read_event_points_live()
        } else {
            None
        };
        let card = decide_target(&cards, explicit, points, &bad_labels)?;
        if let Some(runtime) = self.event_farm_runtime.as_mut() {
            runtime.rescan_needed = false;
        }
        if card.is_story() {
            self.emit("EventMap", &format!("推进主线「{}」", card.name));
        } else {
            self.emit(
                "EventMap",
                &format!("刷活动点数：「{}」等级{}", card.name, card.level_text()),
            );
        }
        Ok(card)
    }

    /// Live point read for the planner. Unreadable points count as 0,
    /// which makes the plan grind the farm quest — the safe direction
    /// (never skip farming in order to rush a locked story node).
    fn read_event_points_live(&mut self) -> Option<u32> {
        match self.sidecar().read_event_points(None) {
            Ok(result) => result.points,
            Err(error) => {
                self.emit_warn("EventMap", &format!("读取活动点数失败，按 0 处理: {error}"));
                None
            }
        }
    }

    /// Before tapping ▶, read the open quest card and compare it with the
    /// card the planner picked from the scan cache. A stale cache (quests
    /// first-cleared since the last scan) would otherwise send the runner
    /// into a card that no longer offers ▶. The live read replaces the
    /// cached entry and the plan re-decides; a changed target closes the
    /// card for a fresh pick. Best-effort: a failed read proceeds with the
    /// cached pick, which is no worse than running without the check.
    fn verify_pending_card(&mut self) -> Result<VerifyOutcome, String> {
        let Some(pending) = self
            .event_farm_runtime
            .as_ref()
            .and_then(|runtime| runtime.pending_card.clone())
        else {
            return Ok(VerifyOutcome::Proceed);
        };
        if self
            .event_farm_runtime
            .as_ref()
            .is_none_or(|runtime| runtime.last_cards.is_empty())
        {
            return Ok(VerifyOutcome::Proceed);
        }
        let result = self
            .sidecar()
            .read_quest_card(None)
            .map_err(|error| format!("任务卡实读失败: {error}"))?;
        if !result.found {
            // Cards fade in; an unreadable frame is not proof of a mismatch.
            return Ok(VerifyOutcome::Proceed);
        }
        let live = QuestCard::from_result(result, pending.label.clone());
        if !cards_differ(&pending, &live) {
            return Ok(VerifyOutcome::Proceed);
        }
        self.emit_warn(
            "NodeDetail",
            &format!(
                "任务卡实读为「{}」等级{} AP{}，与缓存「{}」不一致",
                live.name,
                live.level_text(),
                live.ap,
                pending.name
            ),
        );
        let (target, bad_labels) = self
            .event_farm_runtime
            .as_ref()
            .map(|runtime| {
                (
                    runtime.config.target_quest.clone(),
                    runtime.bad_labels.clone(),
                )
            })
            .unwrap_or((None, HashMap::new()));
        let merged = merge_card(
            self.event_farm_runtime
                .as_ref()
                .map(|runtime| runtime.last_cards.clone())
                .unwrap_or_default(),
            live,
        );
        if let Some(path) = self.scan_cache_path() {
            if let Err(error) = save_cache(&path, merged.clone()) {
                self.emit_warn("NodeDetail", &format!("写入扫图缓存失败: {error}"));
            }
        }
        let attempts = self
            .event_farm_runtime
            .as_mut()
            .map(|runtime| {
                runtime.last_cards = merged.clone();
                runtime.label_attempts += 1;
                runtime.label_attempts
            })
            .unwrap_or(0);
        if attempts > MAX_CARD_ATTEMPTS {
            self.blacklist_pending_label("NodeDetail", "任务卡与缓存反复不一致");
            self.apply_backoff("NodeDetail");
            return Ok(VerifyOutcome::Reselect);
        }
        let points = self.read_event_points_live();
        match decide_target(&merged, target.as_deref(), points, &bad_labels) {
            Ok(decided) if decided.label == pending.label => {
                if let Some(runtime) = self.event_farm_runtime.as_mut() {
                    runtime.pending_card = Some(decided);
                }
                Ok(VerifyOutcome::Proceed)
            }
            Ok(_) => Ok(VerifyOutcome::Reselect),
            Err(error) => {
                self.fail_action("NodeDetail", "校验任务卡后重新决策", error);
                Ok(VerifyOutcome::Reselect)
            }
        }
    }

    fn find_event_label_point(&mut self, text: &str) -> Result<Option<(u32, u32)>, String> {
        if let Some(point) = self.label_point_in_frame(text)? {
            return Ok(Some(point));
        }
        // The label is not in the current view: the game resets map zoom
        // and pan on every return to the map, so the scanned position can
        // be off-screen. Sweep the whole map for the label the way the
        // scanner does, then tap where it was found.
        self.emit_debug("EventMap", &format!("当前视野没有标签「{text}」，拖图搜索…"));
        let client = self.sidecar.as_mut().expect("runner sidecar missing");
        let touch = &mut *self.touch;
        let mut scanner = EventScanner::new(client, touch, (self.screen_w, self.screen_h));
        let found = scanner.find_label(text)?;
        Ok(found.map(|label| map::label_point(&label, (self.screen_w, self.screen_h))))
    }

    fn label_point_in_frame(&mut self, text: &str) -> Result<Option<(u32, u32)>, String> {
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
        let picked = pick_target_card(&cards, Some("残骸小镇"), &HashMap::new()).unwrap();
        assert_eq!(picked.plus, 2);
    }

    #[test]
    fn explicit_target_never_picks_a_locked_card() {
        let cards = [card("残骸小镇", 90, 2, 40, true, "小镇")];
        assert!(pick_target_card(&cards, Some("残骸小镇"), &HashMap::new()).is_err());
    }

    #[test]
    fn empty_target_falls_back_to_the_best_farm_quest() {
        let cards = [
            card("剧情", 10, 0, 5, false, "a"),
            card("刷点本", 90, 1, 40, false, "b"),
            card("高难度", 90, 2, 40, false, "c"),
        ];
        let picked = pick_target_card(&cards, Some("  "), &HashMap::new()).unwrap();
        assert_eq!(picked.name, "刷点本");
        assert_eq!(picked.plus, 1);
    }

    #[test]
    fn no_farmable_card_is_an_error_not_a_story_quest() {
        let cards = [card("序幕", 10, 0, 0, false, "a")];
        let error = pick_target_card(&cards, None, &HashMap::new()).unwrap_err();
        assert!(error.contains("没有可打的活动关卡"));
    }

    #[test]
    fn blacklisted_labels_are_excluded_until_the_limit() {
        let cards = [
            card("刷点本", 90, 1, 40, false, "小镇"),
            card("备用本", 80, 1, 40, false, "港口"),
        ];
        let mut bad = HashMap::new();
        bad.insert("小镇".to_string(), BAD_LIMIT - 1);
        let picked = pick_target_card(&cards, None, &bad).unwrap();
        assert_eq!(picked.label, "小镇");

        bad.insert("小镇".to_string(), BAD_LIMIT);
        let picked = pick_target_card(&cards, None, &bad).unwrap();
        assert_eq!(picked.label, "港口");

        // A named target also skips its blacklisted label.
        assert!(pick_target_card(&cards, Some("刷点本"), &bad).is_err());
    }

    #[test]
    fn backoff_doubles_up_to_the_cap() {
        let expected = [0, 5, 10, 20, 40, 60, 60];
        for (failures, seconds) in expected.iter().enumerate() {
            assert_eq!(backoff_seconds(failures as u32), *seconds);
        }
        assert_eq!(backoff_seconds(50), 60);
    }

    #[test]
    fn cards_differ_only_on_identity_fields() {
        let cached = card("小镇的执念", 90, 2, 40, false, "小镇");
        let mut live = cached.clone();
        assert!(!cards_differ(&cached, &live));

        // The quest at this label changed since the scan: mismatch.
        live = card("新解锁的主线", 10, 0, 5, false, "小镇");
        assert!(cards_differ(&cached, &live));

        // Same quest, different AP or level: mismatch.
        live = card("小镇的执念", 90, 2, 35, false, "小镇");
        assert!(cards_differ(&cached, &live));
        live = card("小镇的执念", 85, 2, 40, false, "小镇");
        assert!(cards_differ(&cached, &live));

        // Bookkeeping fields never decide a mismatch.
        let mut live = card(" 小镇的执念 ", 90, 2, 40, true, "别的标签");
        live.cond = "点数达100以上".into();
        assert!(!cards_differ(&cached, &live));
    }

    #[test]
    fn merge_card_replaces_by_label_and_keeps_the_rest() {
        let cards = vec![card("旧的", 90, 0, 40, false, "小镇"), card("另一本", 80, 0, 30, false, "港口")];
        let merged = merge_card(cards, card("实读", 70, 1, 40, false, "小镇"));
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].name, "实读");
        assert_eq!(merged[1].name, "另一本");

        let merged = merge_card(merged, card("新标签", 60, 0, 40, false, "山丘"));
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[2].name, "新标签");
    }

    #[test]
    fn decide_target_follows_the_explicit_target_first() {
        let cards = [card("剧情", 10, 0, 5, false, "a"), card("刷点本", 90, 1, 40, false, "b")];
        let picked = decide_target(&cards, Some("刷点本"), Some(0), &HashMap::new()).unwrap();
        assert_eq!(picked.name, "刷点本");
    }

    #[test]
    fn decide_target_auto_plan_prefers_unlocked_story() {
        let cards = [
            card("剧情", 10, 0, 5, false, "a"),
            card("刷点本", 90, 1, 40, false, "b"),
        ];
        let picked = decide_target(&cards, None, Some(999_999), &HashMap::new()).unwrap();
        assert_eq!(picked.name, "剧情");

        // With story finished (no story cards left) the plan grinds.
        let cards = [card("刷点本", 90, 1, 40, false, "b")];
        let picked = decide_target(&cards, None, Some(999_999), &HashMap::new()).unwrap();
        assert_eq!(picked.name, "刷点本");
    }

    #[test]
    fn decide_target_errors_when_nothing_is_usable() {
        let cards = [card("刷点本", 90, 1, 40, true, "b")];
        assert!(decide_target(&cards, None, Some(0), &HashMap::new()).is_err());
    }

    #[test]
    fn scan_shows_nothing_locked_requires_a_real_scan_and_zero_locks() {
        // Empty scan = scan failed, never treated as "everything unlocked".
        assert!(!scan_shows_nothing_locked(&[]));
        assert!(scan_shows_nothing_locked(&[
            card("剧情", 10, 0, 5, false, "a"),
            card("刷点本", 90, 1, 40, false, "b"),
        ]));
        assert!(!scan_shows_nothing_locked(&[
            card("剧情", 10, 0, 5, false, "a"),
            card("还没解锁", 90, 0, 40, true, "b"),
        ]));
    }

    #[test]
    fn dialog_points_match_the_reference_1920x1080_coordinates() {
        let to_px = |p: (f64, f64)| {
            (
                (p.0 * 1920.0).round() as u32,
                (p.1 * 1080.0).round() as u32,
            )
        };
        assert_eq!(to_px(NODE_FORWARD), (1370, 320));
        assert_eq!(to_px(QUEST_START_POINT), (1259, 846));
        assert_eq!(to_px(CARD_CLOSE), (163, 63));
        assert_eq!(to_px(SKIP_BUTTON), (1786, 60));
        assert_eq!(to_px(SKIP_CONFIRM_YES), (1241, 840));
        assert_eq!(to_px(TAP_ANYWHERE), (960, 400));
        assert_eq!(to_px(TIP_QUIT), (975, 845));
        assert_eq!(to_px(LIST_UPDATE_NO), (592, 755));
        assert_eq!(to_px(REPEAT_CONFIRM_CLOSE), (700, 875));
        assert_eq!(to_px(SERVANT_FULL_CLOSE), (860, 720));
        assert_eq!(to_px(GAME_NOTICE_CLOSE), (1865, 45));
    }

    #[test]
    fn runtime_starts_waiting_on_the_map_without_a_target() {
        let runtime = EventFarmRuntime::new(EventFarmConfig::default());
        assert_eq!(runtime.phase, EventFarmPhase::NeedMap);
        assert!(runtime.pending_card.is_none());
        assert_eq!(runtime.label_attempts, 0);
        assert!(!runtime.rescan_needed);
        assert_eq!(runtime.failures, 0);
        assert!(runtime.bad_labels.is_empty());
    }
}
