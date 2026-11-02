use super::cache::{ScanCache, SCAN_CACHE_MAX_AGE};
use super::card::{best_farm_quest, QuestCard};
use super::map::{
    hint_ok, is_boundary, label_point, mean_abs_diff, pan_swipe, usable_labels, Axis,
    MapNavigator,
};
use super::scout::{delta_targets, merge_delta};
use crate::screen::MapLabel;
use crate::touch::TouchBackend;

fn card(name: &str, level: u32, plus: u32, ap: u32, locked: bool, label: &str) -> QuestCard {
    QuestCard {
        name: name.to_string(),
        level,
        plus,
        ap,
        locked,
        cond: String::new(),
        label: label.to_string(),
    }
}

fn label(text: &str, cx: f64, cy: f64) -> MapLabel {
    MapLabel {
        text: text.to_string(),
        cx,
        cy,
        x0: cx - 0.05,
        y0: cy - 0.02,
        x1: cx + 0.05,
        y1: cy + 0.02,
        confidence: 0.9,
    }
}

// ---------------------------------------------------------------- cards

#[test]
fn level_suffix_ranks_above_a_plain_level() {
    assert_eq!(card("a", 90, 2, 40, false, "").level_text(), "90++");
    assert_eq!(card("a", 90, 1, 40, false, "").level_text(), "90+");
    assert_eq!(card("a", 90, 0, 40, false, "").level_text(), "90");
    assert!(card("a", 90, 2, 40, false, "").level_key() > card("b", 90, 1, 40, false, "").level_key());
    assert!(card("a", 90, 1, 40, false, "").level_key() > card("b", 90, 0, 40, false, "").level_key());
}

#[test]
fn story_and_farm_classification_follows_ap_and_name() {
    assert!(card("居住于M星的奥林波斯诸神", 90, 0, 10, false, "").is_story());
    assert!(card("主线关卡第五节", 0, 0, 0, true, "").is_story());
    assert!(!card("高难度挑战", 90, 0, 10, false, "").is_story());
    assert!(!card("LIVE FES 特别关卡", 90, 0, 10, false, "").is_story());
    assert!(!card("90++ 刷点本", 90, 2, 40, false, "").is_story());
    assert!(card("90++ 刷点本", 90, 2, 40, false, "").is_farm());
    assert!(!card("主线关卡第五节", 0, 0, 10, true, "").is_farm());
}

#[test]
fn area_info_panels_are_not_quests() {
    assert!(!card("仙女座", 0, 0, 0, false, "").is_real_quest());
    assert!(card("某任务", 0, 0, 1, false, "").is_real_quest());
    assert!(card("主线关卡第七节", 0, 0, 0, true, "").is_real_quest());
    assert!(card("终幕", 0, 0, 0, true, "").is_real_quest());
}

#[test]
fn unlock_threshold_is_read_from_the_condition_text() {
    let mut card = card("主线关卡第五节", 0, 0, 0, true, "");
    card.cond = "开放条件：点数达 12,000 以上".into();
    assert_eq!(card.threshold(), 12_000);
    card.cond = "点数达 8,500以上".into();
    assert_eq!(card.threshold(), 8_500);
    card.cond = "无限制".into();
    assert_eq!(card.threshold(), 0);
    card.cond = "点数达 12,000".into();
    assert_eq!(card.threshold(), 0);
}

#[test]
fn best_farm_quest_prefers_the_highest_unlocked_level_and_skips_challenges() {
    let cards = vec![
        card("普通90", 90, 0, 40, false, "a"),
        card("高级90++", 90, 2, 40, false, "b"),
        card("锁定95", 95, 0, 40, true, "c"),
        card("高难度90+++", 90, 3, 40, false, "d"),
        card("低耗10", 95, 0, 10, false, "e"),
    ];
    assert_eq!(best_farm_quest(&cards).unwrap().name, "高级90++");
    assert!(best_farm_quest(&[card("锁定95", 95, 0, 40, true, "c")]).is_none());
}

#[test]
fn best_farm_quest_keeps_map_order_on_ties() {
    let cards = vec![
        card("先出现", 90, 2, 40, false, "a"),
        card("后出现", 90, 2, 40, false, "b"),
    ];
    assert_eq!(best_farm_quest(&cards).unwrap().name, "先出现");
}

// ---------------------------------------------------------------- map

#[test]
fn pan_swipe_matches_the_reference_geometry_at_1080p() {
    assert_eq!(
        pan_swipe(-1, Axis::X, (1920, 1080)),
        ((1660, 600), (260, 600))
    );
    assert_eq!(
        pan_swipe(1, Axis::X, (1920, 1080)),
        ((260, 600), (1660, 600))
    );
    assert_eq!(
        pan_swipe(-1, Axis::Y, (1920, 1080)),
        ((960, 750), (960, 300))
    );
    assert_eq!(
        pan_swipe(1, Axis::Y, (1920, 1080)),
        ((960, 300), (960, 750))
    );
}

#[test]
fn pan_swipe_scales_to_a_smaller_display() {
    assert_eq!(
        pan_swipe(-1, Axis::X, (1280, 720)),
        ((1106, 400), (174, 400))
    );
    assert_eq!(pan_swipe(-1, Axis::Y, (1280, 720)), ((640, 500), (640, 200)));
}

#[test]
fn frame_difference_detects_the_map_boundary() {
    assert_eq!(mean_abs_diff(&[0, 0, 0], &[0, 0, 0]), Some(0.0));
    assert_eq!(mean_abs_diff(&[0, 0], &[2, 4]), Some(3.0));
    assert_eq!(mean_abs_diff(&[0, 0], &[3, 3]), Some(3.0));
    assert_eq!(mean_abs_diff(&[], &[]), None);
    assert_eq!(mean_abs_diff(&[1], &[1, 2]), None);
    assert!(is_boundary(0.0));
    assert!(is_boundary(2.9));
    assert!(!is_boundary(3.0));
}

#[test]
fn labels_are_deduplicated_by_text_keeping_the_most_confident_read() {
    let mut weak = label("仙女座", 0.3, 0.2);
    weak.confidence = 0.4;
    let mut strong = label(" 仙女座 ", 0.31, 0.21);
    strong.confidence = 0.95;
    let other = label("废弃教堂", 0.6, 0.5);

    let labels = usable_labels(&[weak, strong, other]);
    assert_eq!(labels.len(), 2);
    assert_eq!(labels[0].text, "仙女座");
    assert_eq!(labels[0].confidence, 0.95);
    assert_eq!(labels[1].text, "废弃教堂");
}

#[test]
fn hint_matching_tolerates_ocr_drift_but_not_unrelated_names() {
    assert!(hint_ok("仙女座遗迹", "仙女座"));
    assert!(hint_ok("仙女座", "仙女座遗迹群"));
    assert!(hint_ok("仙士座遗迹", "仙女座遗迹"));
    assert!(!hint_ok("仙女座", "废弃教堂"));
    assert!(!hint_ok("仙女座", ""));
}

#[test]
fn label_center_projects_to_device_pixels() {
    let label = label("仙女座", 0.5, 0.5);
    assert_eq!(label_point(&label, (1920, 1080)), (960, 540));
    assert_eq!(label_point(&label, (100, 100)), (50, 50));
}

struct RecordingTouch {
    calls: Vec<String>,
}

impl TouchBackend for RecordingTouch {
    fn tap(&mut self, _: u32, _: u32) -> Result<(), String> {
        Ok(())
    }
    fn swipe(&mut self, _: (u32, u32), _: (u32, u32), _: u32) -> Result<(), String> {
        Ok(())
    }
    fn swipe_with_settle(
        &mut self,
        from: (u32, u32),
        to: (u32, u32),
        swipe_ms: u32,
        settle_ms: u32,
    ) -> Result<(), String> {
        self.calls
            .push(format!("swipe {from:?}->{to:?} {swipe_ms}/{settle_ms}"));
        Ok(())
    }
    fn pinch(
        &mut self,
        center: (u32, u32),
        from_gap: u32,
        to_gap: u32,
        duration_ms: u32,
    ) -> Result<(), String> {
        self.calls
            .push(format!("pinch {center:?} {from_gap}->{to_gap} {duration_ms}"));
        Ok(())
    }
    fn name(&self) -> &'static str {
        "recording"
    }
}

#[test]
fn navigator_pans_with_settle_and_pinches_the_screen_center() {
    let mut touch = RecordingTouch { calls: Vec::new() };
    {
        let mut navigator = MapNavigator::new(&mut touch, (1920, 1080));
        navigator.pan(-1, Axis::X).unwrap();
        navigator.pan(1, Axis::Y).unwrap();
        navigator.zoom_out().unwrap();
    }
    assert_eq!(
        touch.calls,
        vec![
            "swipe (1660, 600)->(260, 600) 800/180",
            "swipe (960, 300)->(960, 750) 800/180",
            "pinch (960, 540) 600->120 300",
        ]
    );
}

// ---------------------------------------------------------------- cache

#[test]
fn scan_cache_expires_after_three_hours() {
    let cache = ScanCache::new(vec![card("a", 90, 0, 40, false, "x")], 1_000);
    assert!(cache.is_fresh(1_000, SCAN_CACHE_MAX_AGE));
    assert!(cache.is_fresh(1_000 + SCAN_CACHE_MAX_AGE, SCAN_CACHE_MAX_AGE));
    assert!(!cache.is_fresh(
        1_001 + SCAN_CACHE_MAX_AGE,
        SCAN_CACHE_MAX_AGE
    ));
    // Clock skew must not make a cache look fresh forever.
    assert!(cache.is_fresh(0, SCAN_CACHE_MAX_AGE));
}

#[test]
fn scan_cache_round_trips_and_rejects_legacy_entries_without_plus() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("scout.json");
    let cache = ScanCache::new(vec![card("90++ 刷点本", 90, 2, 40, false, "仙女座")], 42);
    cache.save(&path).unwrap();
    assert_eq!(ScanCache::load(&path), Some(cache));

    std::fs::write(
        &path,
        r#"{"scannedAt":42,"cards":[{"name":"a","level":90,"ap":40,"locked":false,"cond":"","label":"x"}]}"#,
    )
    .unwrap();
    assert_eq!(ScanCache::load(&path), None);
    assert_eq!(ScanCache::load(&dir.path().join("missing.json")), None);
}

// ---------------------------------------------------------------- delta scan

#[test]
fn delta_targets_read_new_and_locked_labels_only() {
    let previous = vec![
        card("已解锁", 90, 0, 40, false, "仙女座"),
        card("还锁着", 0, 0, 0, true, "废弃教堂"),
    ];
    let labels = vec![
        label("仙女座", 0.3, 0.2),
        label("废弃教堂", 0.6, 0.5),
        label("新区域", 0.4, 0.7),
    ];
    let targets: Vec<&str> = delta_targets(&previous, &labels)
        .iter()
        .map(|label| label.text.as_str())
        .collect();
    assert_eq!(targets, vec!["废弃教堂", "新区域"]);
}

#[test]
fn merge_delta_keeps_the_unlocked_card_under_a_relocked_label() {
    let previous = vec![
        card("主线关卡第五节", 0, 0, 0, true, "仙女座"),
        card("别的区域", 90, 0, 40, false, "废弃教堂"),
    ];
    let added = vec![card("真正的任务名", 90, 0, 40, false, "仙女座")];
    let targets = vec![label("仙女座", 0.3, 0.2)];

    let merged = merge_delta(previous, added, &targets);
    assert_eq!(merged.len(), 2);
    assert!(merged.iter().any(|card| card.name == "真正的任务名"));
    assert!(merged.iter().any(|card| card.name == "别的区域"));
    assert!(!merged.iter().any(|card| card.name == "主线关卡第五节"));
}

#[test]
fn merge_delta_updates_a_known_card_and_appends_new_ones() {
    let previous = vec![card("同名", 90, 0, 40, false, "仙女座")];
    let added = vec![
        card("同名", 90, 2, 40, false, "仙女座"),
        card("新卡", 80, 0, 40, false, "新区域"),
    ];
    let merged = merge_delta(previous, added, &[]);
    assert_eq!(merged.len(), 2);
    assert_eq!(merged[0].level_text(), "90++");
    assert_eq!(merged[1].name, "新卡");
}
