//! Debug: scan the event map and cache the quest cards.
//!
//! This is the M1 verification entry point. It assumes the game is
//! already showing the event map; navigating there from the terminal is
//! the automation runtime's job.

use super::*;
use crate::runner::{best_farm_quest, save_cache, EventScanner, QuestCard};
use crate::touch;

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugEventScanResult {
    labels: Vec<String>,
    cards: Vec<QuestCard>,
    points: Option<u32>,
    best: Option<QuestCard>,
    cached: bool,
    screen_size: DebugScreenSize,
}

#[tauri::command]
pub fn debug_scan_event_map(
    app: tauri::AppHandle,
    adb_settings_state: tauri::State<'_, Mutex<AdbDeviceSettings>>,
    server_state: tauri::State<'_, Mutex<Server>>,
    debug_state: tauri::State<'_, DebugSidecar>,
    handle_state: tauri::State<'_, Mutex<RunnerHandle>>,
    enhancement_handle_state: tauri::State<'_, Mutex<EnhancementRunnerHandle>>,
    ce_enhancement_handle_state: tauri::State<'_, Mutex<CraftEssenceEnhancementRunnerHandle>>,
    friend_point_summon_handle_state: tauri::State<'_, Mutex<FriendPointSummonRunnerHandle>>,
) -> Result<DebugEventScanResult, String> {
    require_automation_idle(
        &handle_state,
        &enhancement_handle_state,
        &ce_enhancement_handle_state,
        &friend_point_summon_handle_state,
    )?;

    let selected_adb_serial = adb_settings_state
        .lock()
        .unwrap()
        .selected_adb_serial
        .clone();
    let server = current_server(&server_state);
    let mut adb_dev = adb::Adb::new(&app, selected_adb_serial);
    adb_dev.connect_for_game()?;
    let screen = adb_dev
        .screen_size()
        .ok_or_else(|| "无法读取设备分辨率".to_string())?;
    ensure_debug_stream(&app, &debug_state, server, &adb_dev)?;

    let mut guard = debug_state.0.lock().unwrap();
    let client = guard
        .as_mut()
        .ok_or_else(|| "debug sidecar not initialized".to_string())?;
    // The scan is the only consumer of the event detection scope until
    // the automation runtime starts using it.
    client.use_event_detection();

    let mut touch = touch::build(&adb_dev, client, &app, screen);
    let outcome = {
        let mut scanner = EventScanner::new(client, touch.as_mut(), screen);
        if !scanner.wait_for_map()? {
            return Err("当前不在活动地图，请先手动进入活动地图再扫描".into());
        }
        scanner.scan()?
    };

    let best = best_farm_quest(&outcome.cards).cloned();
    let cached = save_cache(&scan_cache_path(&app), outcome.cards.clone()).is_ok();
    eprintln!(
        "[debug_scan_event_map] labels={} cards={} points={:?} cached={cached}",
        outcome.labels.len(),
        outcome.cards.len(),
        outcome.points
    );
    Ok(DebugEventScanResult {
        labels: outcome.labels,
        cards: outcome.cards,
        points: outcome.points,
        best,
        cached,
        screen_size: DebugScreenSize {
            w: screen.0,
            h: screen.1,
        },
    })
}

fn scan_cache_path(app: &tauri::AppHandle) -> std::path::PathBuf {
    crate::app_data_dir(app).join("event").join("scout.json")
}
