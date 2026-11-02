//! Headless event-farm device harness.
//!
//! Drives the M1/M2 acceptance flow straight against a device — no GUI app,
//! no Tauri runtime: ADB for the device and screenshots, the mash-cv sidecar
//! spawned directly over stdio, the ADB touch backend for input. Reuses the
//! production scanner (`EventScanner`) and farm selection primitives
//! (`pick_target_card`) so what is verified here is the code that ships.
//!
//! Phases:
//! * `scan`  — wait for the event map, zoom out, scan every label, print the
//!   cards/points/labels as JSON (M1 acceptance).
//! * `enter` — pick the best quest, tap its label, ▶, 开始任务, and wait for
//!   the support-select handoff point (M2 acceptance; battle not driven here).
//! * `loop`  — full farm loop incl. battle: pending (M4 soak), rejected for now.
//!
//! Run from the repo root:
//! `cargo run --bin event_harness -- --phase scan [--serial 127.0.0.1:16384]`

use crate::adb::Adb;
use crate::runner::{
    hint_ok, label_point, pick_target_card, plan_once, usable_labels, EventPlan, EventScanner,
    NODE_FORWARD, QuestCard, QUEST_START_POINT,
};
use crate::screen::{
    BATTLE_DETECTION_SCREENS, CvError, EVENT_ONLY_DETECTION_SCREENS, EventCvSource,
    EventPointsResult, FrameThumbnail, MapLabelsResult, Point, QuestCardResult, Screen,
};
use crate::Server;
use serde::Serialize;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// Between-poll delay while waiting for a screen to appear.
const POLL_INTERVAL: Duration = Duration::from_millis(1_000);
/// Longest wait for the event map to show up after launch.
const MAP_WAIT_TIMEOUT: Duration = Duration::from_secs(120);
/// Longest wait for the support/confirm screens after 开始任务.
const HANDOFF_WAIT_TIMEOUT: Duration = Duration::from_secs(45);
/// MuMu 12's default ADB port, then the older MuMu port.
const MUMU_PORTS: [&str; 2] = ["16384", "7555"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessPhase {
    Scan,
    Enter,
}

pub struct HarnessOptions {
    pub serial: Option<String>,
    pub phase: HarnessPhase,
    pub target_quest: Option<String>,
    pub out_dir: PathBuf,
}

/// Parse `--serial <s>`, `--phase <scan|enter>`, `--target <name>`,
/// `--out-dir <dir>`. Returns Err with usage on bad input.
pub fn parse_args(args: &[String]) -> Result<HarnessOptions, String> {
    let mut serial = None;
    let mut phase = None;
    let mut target_quest = None;
    let mut out_dir = None;
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        let Some(value) = args.get(i + 1) else {
            return Err(format!("缺少 {flag} 的参数值"));
        };
        match flag {
            "--serial" => serial = Some(value.clone()),
            "--phase" => {
                phase = Some(match value.as_str() {
                    "scan" => HarnessPhase::Scan,
                    "enter" => HarnessPhase::Enter,
                    other => return Err(format!("未知阶段 {other}（支持 scan / enter）")),
                });
            }
            "--target" => target_quest = Some(value.clone()),
            "--out-dir" => out_dir = Some(PathBuf::from(value)),
            other => return Err(format!("未知参数 {other}")),
        }
        i += 2;
    }
    Ok(HarnessOptions {
        serial,
        phase: phase.ok_or("缺少 --phase（scan / enter）")?,
        target_quest,
        out_dir: out_dir.unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .map(|root| root.join(".screenshots"))
                .unwrap_or_else(|| PathBuf::from(".screenshots"))
        }),
    })
}

/// All on-disk locations the harness needs. Every one is resolved from the
/// dev source tree or the installed app's data dir; nothing is bundled.
struct HarnessPaths {
    adb_exe: PathBuf,
    runtime_exe: PathBuf,
    models_dir: PathBuf,
    code_dir: PathBuf,
    servants_json: PathBuf,
    template_dirs: Vec<(PathBuf, Option<&'static str>)>,
    config_paths: Vec<PathBuf>,
    scrcpy_jar: PathBuf,
}

fn dev_resource(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources").join(name)
}

fn app_data_root() -> Result<PathBuf, String> {
    if let Some(dir) = std::env::var_os("MASH_APP_DATA") {
        return Ok(PathBuf::from(dir));
    }
    // The GUI app's data dir: Tauri's app_data_dir for the identifier.
    let appdata = std::env::var("APPDATA")
        .map_err(|_| "无法定位 %APPDATA%；用 MASH_APP_DATA 指定应用数据目录".to_string())?;
    Ok(PathBuf::from(appdata).join("com.xiaotongx.mash"))
}

fn resolve_paths(server: Server) -> Result<HarnessPaths, String> {
    let token = server.dir_token();
    let adb_exe = dev_resource("adb").join(if cfg!(windows) { "adb.exe" } else { "adb" });
    if !adb_exe.is_file() {
        return Err(format!("找不到 adb: {}", adb_exe.display()));
    }
    let scrcpy_jar = dev_resource("scrcpy").join("scrcpy-server.jar");
    if !scrcpy_jar.is_file() {
        return Err(format!("找不到 scrcpy-server.jar: {}", scrcpy_jar.display()));
    }
    let code_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| root.join("sidecar").join("cv-code"))
        .filter(|dir| dir.join("mash_cv").is_dir())
        .ok_or("找不到 sidecar/cv-code 代码目录")?;

    // Installed CV runtime under the app data dir, same layout the GUI app
    // resolves: runtime/mash-cv/runtime/<version>/mash-cv-runtime/.
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dev_resource("runtime-manifest.json"))
            .map_err(|e| format!("读取 runtime-manifest.json 失败: {e}"))?,
    )
    .map_err(|e| format!("解析 runtime-manifest.json 失败: {e}"))?;
    let platform_key = format!(
        "{}-{}",
        match std::env::consts::OS {
            "macos" => "darwin",
            other => other,
        },
        std::env::consts::ARCH
    );
    let version = manifest["platforms"][&platform_key]["runtimeVersion"]
        .as_str()
        .ok_or_else(|| format!("runtime manifest 缺少 {platform_key} 的版本"))?;
    let app_data = app_data_root()?;
    let runtime_dir = app_data
        .join("runtime")
        .join("mash-cv")
        .join("runtime")
        .join(version)
        .join("mash-cv-runtime");
    let runtime_exe = runtime_dir.join(if cfg!(windows) { "mash-cv.exe" } else { "mash-cv" });
    if !runtime_exe.is_file() {
        return Err(format!(
            "未安装 mash-cv runtime ({}): 先在 GUI 里下载 CV 运行时，或用 MASH_APP_DATA 指向安装目录",
            runtime_exe.display()
        ));
    }
    let models_dir = runtime_dir.join("_internal").join("mash_cv").join("models");
    if !models_dir.is_dir() {
        return Err(format!("未找到 OCR 模型目录: {}", models_dir.display()));
    }
    let servants_json = app_data.join("assets").join("catalogs").join("servants.json");
    if !servants_json.is_file() {
        return Err(format!("未找到从者元数据: {}", servants_json.display()));
    }

    let template_dirs = vec![
        (
            dev_resource("servers").join("shared").join("templates"),
            Some("shared"),
        ),
        (
            dev_resource("servers").join(token).join("templates"),
            None,
        ),
    ];
    for (dir, _) in &template_dirs {
        if !dir.is_dir() {
            return Err(format!("模板目录缺失: {}", dir.display()));
        }
    }
    let config_paths = vec![
        dev_resource("servers").join("shared").join("cv.json"),
        dev_resource("servers").join(token).join("cv.json"),
    ];
    for path in &config_paths {
        if !path.is_file() {
            return Err(format!("cv 配置缺失: {}", path.display()));
        }
    }

    Ok(HarnessPaths {
        adb_exe,
        runtime_exe,
        models_dir,
        code_dir,
        servants_json,
        template_dirs,
        config_paths,
        scrcpy_jar,
    })
}

// ---------------------------------------------------------------------------
// Minimal sidecar client: same JSON-line protocol, std::process instead of
// the Tauri shell plugin.
// ---------------------------------------------------------------------------

struct HarnessSidecar {
    child: Child,
    line_rx: mpsc::Receiver<String>,
    next_id: u64,
    screen_names: Vec<String>,
}

impl HarnessSidecar {
    fn spawn(paths: &HarnessPaths, server: Server) -> Result<Self, String> {
        let mut cmd = Command::new(&paths.runtime_exe);
        cmd.env("MASH_CV_CODE_DIR", &paths.code_dir)
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .env("MASH_CV_MODELS_DIR", &paths.models_dir)
            .env("MASH_CV_SERVANTS_JSON_PATH", &paths.servants_json)
            .env("PYTHONPATH", &paths.code_dir)
            .env("PYTHONIOENCODING", "utf-8")
            .env("PYTHONUTF8", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("启动 sidecar 失败 ({}): {e}", paths.runtime_exe.display()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "sidecar stdout 不可读".to_string())?;
        let (tx, rx) = mpsc::channel::<String>();
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut sidecar = Self {
            child,
            line_rx: rx,
            next_id: 1,
            screen_names: Vec::new(),
        };
        // Cold boot (PyInstaller extraction, OpenCV import) can take a while.
        sidecar.request(serde_json::json!({"cmd": "ping"}), Duration::from_secs(90))
            .map_err(|e| format!("sidecar 未就绪: {e}"))?;
        for (idx, (dir, prefix)) in paths.template_dirs.iter().enumerate() {
            let context = format!("load_templates({})", dir.display());
            let resp = sidecar
                .request(
                    serde_json::json!({
                        "cmd": "load_templates",
                        "dir": dir.to_string_lossy(),
                        "append": idx > 0,
                        "keyPrefix": prefix.unwrap_or(""),
                    }),
                    Duration::from_secs(30),
                )
                .map_err(|e| format!("{context}: {e}"))?;
            sidecar.require_ok(&resp, &context)?;
        }
        for (idx, path) in paths.config_paths.iter().enumerate() {
            let context = format!("load_config({})", path.display());
            let resp = sidecar
                .request(
                    serde_json::json!({
                        "cmd": "load_config",
                        "path": path.to_string_lossy(),
                        "merge": idx > 0,
                    }),
                    Duration::from_secs(30),
                )
                .map_err(|e| format!("{context}: {e}"))?;
            sidecar.require_ok(&resp, &context)?;
        }
        sidecar.request(
            serde_json::json!({"cmd": "set_server", "server": server.to_string()}),
            Duration::from_secs(10),
        )
        .map_err(|e| format!("set_server: {e}"))?;
        sidecar.screen_names = BATTLE_DETECTION_SCREENS
            .iter()
            .chain(EVENT_ONLY_DETECTION_SCREENS.iter())
            .map(|name| name.to_string())
            .collect();
        Ok(sidecar)
    }

    /// Start the scrcpy video stream; returns the negotiated frame size.
    fn start_stream(&mut self, adb: &Adb, jar: &Path) -> Result<(u32, u32), String> {
        let mut req = serde_json::json!({
            "cmd": "start_stream",
            "adbPath": adb.path().to_string_lossy(),
            "jarPath": jar.to_string_lossy(),
            "maxSize": crate::STREAM_MAX_SIZE,
            "bitRate": crate::STREAM_BIT_RATE,
            "maxFps": crate::STREAM_MAX_FPS,
            "displayId": adb.display_id(),
            "control": false,
        });
        if let Some(serial) = adb.serial() {
            req["serial"] = serde_json::Value::String(serial.to_string());
        }
        let resp = self.request(req, Duration::from_secs(45))?;
        if resp["ok"].as_bool() != Some(true) {
            return Err(format!(
                "start_stream 失败: {}",
                resp["error"].as_str().unwrap_or("未知错误")
            ));
        }
        let w = resp["width"].as_u64().unwrap_or(0) as u32;
        let h = resp["height"].as_u64().unwrap_or(0) as u32;
        if w == 0 || h == 0 {
            return Err("start_stream 返回了无效分辨率".into());
        }
        Ok((w, h))
    }

    /// Tag with a monotonic id, write, and wait for the matching response.
    fn request(
        &mut self,
        mut req: serde_json::Value,
        timeout: Duration,
    ) -> Result<serde_json::Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        req["id"] = serde_json::Value::from(id);
        let stdin = self
            .child
            .stdin
            .as_mut()
            .ok_or_else(|| "sidecar stdin 已关闭".to_string())?;
        let mut line = req.to_string();
        line.push('\n');
        stdin
            .write_all(line.as_bytes())
            .map_err(|e| format!("写入 sidecar 失败: {e}"))?;
        stdin.flush().ok();

        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(|| format!("sidecar 响应超时（id={id}）"))?;
            let response = self
                .line_rx
                .recv_timeout(remaining)
                .map_err(|_| format!("sidecar 响应超时（id={id}）"))?;
            let parsed: serde_json::Value = serde_json::from_str(&response)
                .map_err(|e| format!("sidecar 返回非法 JSON: {e}: {response}"))?;
            match parsed.get("id").and_then(|v| v.as_u64()) {
                Some(rid) if rid == id => return Ok(parsed),
                Some(_) => continue,
                None => {
                    return Err(format!(
                        "sidecar 响应缺少 id（sidecar 版本过旧？）: {response}"
                    ))
                }
            }
        }
    }

    fn require_ok(&self, resp: &serde_json::Value, context: &str) -> Result<(), String> {
        if let Some(error) = resp.get("error").and_then(|v| v.as_str()) {
            return Err(format!("{context}: {error}"));
        }
        if resp["ok"].as_bool() == Some(true) {
            Ok(())
        } else {
            Err(format!("{context}: sidecar 未报告成功"))
        }
    }
}

impl Drop for HarnessSidecar {
    fn drop(&mut self) {
        let _ = self
            .request(serde_json::json!({"cmd": "quit"}), Duration::from_secs(3));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl EventCvSource for HarnessSidecar {
    fn detect(&mut self, _image_path: Option<&Path>) -> Result<Screen, CvError> {
        let resp = self
            .request(
                serde_json::json!({"cmd": "detect", "diagnostics": false, "screenNames": self.screen_names}),
                Duration::from_secs(10),
            )
            .map_err(|e| format!("detect: {e}"))?;
        if let Some(error) = resp.get("error").and_then(|v| v.as_str()) {
            return Err(format!("detect: {error}").into());
        }
        let label = resp["screen"].as_str().unwrap_or("Unknown");
        Ok(label.parse::<Screen>().unwrap_or(Screen::Unknown))
    }

    fn read_map_labels(&mut self, _image_path: Option<&Path>) -> Result<MapLabelsResult, CvError> {
        let resp = self
            .request(serde_json::json!({"cmd": "read_map_labels"}), Duration::from_secs(60))
            .map_err(|e| format!("read_map_labels: {e}"))?;
        if let Some(error) = resp.get("error").and_then(|v| v.as_str()) {
            return Err(format!("read_map_labels: {error}").into());
        }
        serde_json::from_value(resp).map_err(|e| format!("read_map_labels 响应异常: {e}").into())
    }

    fn read_quest_card(&mut self, _image_path: Option<&Path>) -> Result<QuestCardResult, CvError> {
        let resp = self
            .request(serde_json::json!({"cmd": "read_quest_card"}), Duration::from_secs(60))
            .map_err(|e| format!("read_quest_card: {e}"))?;
        if let Some(error) = resp.get("error").and_then(|v| v.as_str()) {
            return Err(format!("read_quest_card: {error}").into());
        }
        serde_json::from_value(resp).map_err(|e| format!("read_quest_card 响应异常: {e}").into())
    }

    fn read_event_points(
        &mut self,
        _image_path: Option<&Path>,
    ) -> Result<EventPointsResult, CvError> {
        let resp = self
            .request(serde_json::json!({"cmd": "read_event_points"}), Duration::from_secs(60))
            .map_err(|e| format!("read_event_points: {e}"))?;
        if let Some(error) = resp.get("error").and_then(|v| v.as_str()) {
            return Err(format!("read_event_points: {error}").into());
        }
        serde_json::from_value(resp).map_err(|e| format!("read_event_points 响应异常: {e}").into())
    }

    fn frame_thumbnail(
        &mut self,
        _image_path: Option<&Path>,
        width: u32,
        height: u32,
    ) -> Result<FrameThumbnail, CvError> {
        let resp = self
            .request(
                serde_json::json!({"cmd": "frame_thumbnail", "width": width, "height": height}),
                Duration::from_secs(10),
            )
            .map_err(|e| format!("frame_thumbnail: {e}"))?;
        if let Some(error) = resp.get("error").and_then(|v| v.as_str()) {
            return Err(format!("frame_thumbnail: {error}").into());
        }
        serde_json::from_value(resp).map_err(|e| format!("frame_thumbnail 响应异常: {e}").into())
    }
}

// ---------------------------------------------------------------------------
// Device + phase drivers
// ---------------------------------------------------------------------------

/// `adb connect` the MuMu ports in turn, then pick a ready device.
fn connect_device(paths: &HarnessPaths, serial: Option<&str>) -> Result<Adb, String> {
    let adb = Adb::with_path(paths.adb_exe.clone(), serial.map(str::to_string));
    if serial.is_none() {
        for port in MUMU_PORTS {
            let target = format!("127.0.0.1:{port}");
            let output = Command::new(&paths.adb_exe)
                .args(["connect", &target])
                .output()
                .map_err(|e| format!("adb connect 执行失败: {e}"))?;
            eprintln!(
                "[harness] adb connect {target}: {}",
                String::from_utf8_lossy(&output.stdout).trim()
            );
        }
    }
    let mut adb = adb;
    adb.connect().map_err(|e| format!("adb 连接失败: {e}"))?;
    eprintln!("[harness] 设备已连接: {:?}", adb.serial());
    Ok(adb)
}

fn save_screenshot(adb: &Adb, out_dir: &Path, name: &str) -> Result<PathBuf, String> {
    std::fs::create_dir_all(out_dir).map_err(|e| format!("创建截图目录失败: {e}"))?;
    let tmp = adb.screenshot_to_file()?;
    let dest = out_dir.join(format!("{name}.png"));
    std::fs::copy(&tmp, &dest).map_err(|e| format!("保存截图失败: {e}"))?;
    let _ = std::fs::remove_file(&tmp);
    eprintln!("[harness] 截图: {}", dest.display());
    Ok(dest)
}

fn wait_for_screen(
    sidecar: &mut HarnessSidecar,
    wanted: &[Screen],
    timeout: Duration,
) -> Result<Screen, String> {
    let deadline = Instant::now() + timeout;
    let mut last = Screen::Unknown;
    while Instant::now() < deadline {
        last = sidecar.detect(None).map_err(|e| e.to_string())?;
        if wanted.contains(&last) {
            return Ok(last);
        }
        thread::sleep(POLL_INTERVAL);
    }
    Err(format!(
        "等待 {:?} 超时，最后一帧是 {last}",
        wanted.first().unwrap_or(&Screen::Unknown)
    ))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ScanReport {
    labels: Vec<String>,
    points: Option<u32>,
    cards: Vec<QuestCard>,
}

fn run_scan(
    sidecar: &mut HarnessSidecar,
    touch: &mut dyn crate::touch::TouchBackend,
    adb: &Adb,
    screen: (u32, u32),
    out_dir: &Path,
) -> Result<ScanReport, String> {
    eprintln!("[harness] 等待活动地图…");
    wait_for_screen(sidecar, &[Screen::EventMap], MAP_WAIT_TIMEOUT)?;
    save_screenshot(adb, out_dir, "harness-scan-before")?;
    let mut scanner = EventScanner::new(sidecar, touch, screen);
    let outcome = scanner
        .scan()
        .map_err(|e| format!("地图扫描失败: {e}"))?;
    save_screenshot(adb, out_dir, "harness-scan-after")?;
    let report = ScanReport {
        labels: outcome.labels,
        points: outcome.points,
        cards: outcome.cards,
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|e| format!("序列化报告失败: {e}"))?
    );
    Ok(report)
}

fn run_enter(
    sidecar: &mut HarnessSidecar,
    touch: &mut dyn crate::touch::TouchBackend,
    adb: &Adb,
    screen: (u32, u32),
    out_dir: &Path,
    target_quest: Option<&str>,
) -> Result<serde_json::Value, String> {
    let report = run_scan(sidecar, touch, adb, screen, out_dir)?;
    let (w, h) = screen;

    let card = match target_quest.filter(|t| !t.trim().is_empty()) {
        Some(target) => pick_target_card(&report.cards, Some(target), &Default::default())
            .map_err(|e| format!("按名选卡失败: {e}"))?,
        None => match plan_once(&report.cards, report.points, &Default::default())
            .map_err(|e| format!("选卡失败: {e}"))?
        {
            EventPlan::Story(card) | EventPlan::Farm(card) => card,
        },
    };
    eprintln!(
        "[harness] 目标关卡「{}」等级{} AP{}（区域 {}）",
        card.name,
        card.level_text(),
        card.ap,
        card.label
    );

    let labels = sidecar
        .read_map_labels(None)
        .map_err(|e| format!("区域标签识别失败: {e}"))?;
    let found = usable_labels(&labels.labels)
        .into_iter()
        .find(|label| label.text.trim() == card.label.trim())
        .or_else(|| {
            usable_labels(&labels.labels)
                .into_iter()
                .find(|label| hint_ok(&label.text, &card.label))
        })
        .ok_or_else(|| format!("地图上找不到标签「{}」", card.label))?;
    let (x, y) = label_point(&found, screen);
    touch.tap(x, y).map_err(|e| format!("点击标签失败: {e}"))?;
    save_screenshot(adb, out_dir, "harness-enter-label-tapped")?;

    wait_for_screen(sidecar, &[Screen::NodeDetail, Screen::Gate], Duration::from_secs(15))?;
    if sidecar.detect(None).map_err(|e| e.to_string())? == Screen::Gate {
        return Err("点标签后进入了自由本列表（Gate），选卡与标签不一致".into());
    }
    save_screenshot(adb, out_dir, "harness-enter-card")?;
    let (fx, fy) = Point::new(NODE_FORWARD.0, NODE_FORWARD.1).to_physical(w, h);
    touch
        .tap(fx, fy)
        .map_err(|e| format!("点击 ▶ 失败: {e}"))?;

    let reached = wait_for_screen(
        sidecar,
        &[Screen::QuestConfirm, Screen::SupportSelect, Screen::TeamConfirm],
        Duration::from_secs(20),
    )?;
    if reached == Screen::QuestConfirm {
        save_screenshot(adb, out_dir, "harness-enter-quest-confirm")?;
        let (sx, sy) = Point::new(QUEST_START_POINT.0, QUEST_START_POINT.1).to_physical(w, h);
        touch
            .tap(sx, sy)
            .map_err(|e| format!("点击「开始任务」失败: {e}"))?;
        wait_for_screen(
            sidecar,
            &[Screen::SupportSelect, Screen::TeamConfirm],
            HANDOFF_WAIT_TIMEOUT,
        )?;
    }
    save_screenshot(adb, out_dir, "harness-enter-handoff")?;
    eprintln!("[harness] 已到达助战/编队交接点，战斗不在 harness 范围");
    Ok(serde_json::json!({
        "ok": true,
        "quest": card.name,
        "level": card.level_text(),
        "ap": card.ap,
    }))
}

/// Entry point for the bin target. Returns the process exit code.
pub fn run_from_env() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let options = match parse_args(&args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{error}\n用法: event_harness --phase <scan|enter> [--serial <s>] [--target <关卡名>] [--out-dir <dir>]");
            return 2;
        }
    };
    match run(options) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("[harness] 失败: {error}");
            1
        }
    }
}

fn run(options: HarnessOptions) -> Result<(), String> {
    if cfg!(not(target_os = "windows")) && std::env::var_os("MASH_APP_DATA").is_none() {
        // Not fatal: the app-data layout is identical on other hosts, this is
        // only a reminder that the harness is exercised on Windows + MuMu.
        eprintln!("[harness] 提示: harness 只在 Windows + MuMu 上验证过");
    }
    let server = Server::Cn;
    let paths = resolve_paths(server)?;
    let adb = connect_device(&paths, options.serial.as_deref())?;
    let mut sidecar = HarnessSidecar::spawn(&paths, server)?;
    eprintln!("[harness] sidecar 已就绪");
    let (w, h) = sidecar
        .start_stream(&adb, &paths.scrcpy_jar)
        .map_err(|e| format!("启动视频流失败: {e}"))?;
    eprintln!("[harness] 视频流: {w}x{h}");
    let mut touch = crate::touch::build_adb_humanized(&adb, (w, h));

    match options.phase {
        HarnessPhase::Scan => {
            run_scan(&mut sidecar, &mut *touch, &adb, (w, h), &options.out_dir)?;
        }
        HarnessPhase::Enter => {
            run_enter(
                &mut sidecar,
                &mut *touch,
                &adb,
                (w, h),
                &options.out_dir,
                options.target_quest.as_deref(),
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arg(name: &str, value: &str) -> String {
        [name.to_string(), value.to_string()].concat()
    }

    #[test]
    fn parse_args_requires_a_known_phase() {
        let args = vec!["--phase".into(), "scan".into()];
        let options = parse_args(&args).unwrap();
        assert_eq!(options.phase, HarnessPhase::Scan);
        assert!(options.serial.is_none());
        assert!(options.target_quest.is_none());
        assert!(options.out_dir.ends_with(".screenshots"));
    }

    #[test]
    fn parse_args_rejects_unknown_flags_and_phases() {
        assert!(parse_args(&["--bogus".into(), "x".into()]).is_err());
        assert!(parse_args(&[arg("--phase", "loop")]).is_err());
        assert!(parse_args(&["--serial".into(), "1.2.3.4:5555".into()]).is_err());
    }

    #[test]
    fn parse_args_keeps_serial_target_and_out_dir() {
        let args = vec![
            "--phase".into(),
            "enter".into(),
            "--serial".into(),
            "127.0.0.1:16384".into(),
            "--target".into(),
            "残骸小镇".into(),
            "--out-dir".into(),
            "C:\\tmp".into(),
        ];
        let options = parse_args(&args).unwrap();
        assert_eq!(options.phase, HarnessPhase::Enter);
        assert_eq!(options.serial.as_deref(), Some("127.0.0.1:16384"));
        assert_eq!(options.target_quest.as_deref(), Some("残骸小镇"));
        assert_eq!(options.out_dir, PathBuf::from("C:\\tmp"));
    }
}
