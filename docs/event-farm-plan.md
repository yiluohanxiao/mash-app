# 活动自动刷本：实施计划（M0 之前）

> 状态：**草案，等待确认**。按 `reference/fgo-auto/REFACTOR-TO-MASH.md` 第 7 节的要求，
> 先给计划，不写实现。
>
> 基线：mash-app `main` @ `1265fb9`（0.27.0），Windows + MuMu 12 + FGO 国服 1920×1080。
> 参考包：`reference/fgo-auto/`（已加入 `.gitignore`）。

---

## 0. 环境现状（已核实）

| 项 | 状态 |
| --- | --- |
| fork 根目录 | `D:\AI Program\fork mash`，mash-app 0.27.0，工作区干净 |
| 参考代码 | `reference/fgo-auto/`，153 个文件 / 110 MB，含 `tests/corpus` 51 张标注图 |
| Node / pnpm | v22.23.4 / 10.32.0（可用，`node_modules` 尚未安装） |
| Rust | **未安装**（`cargo` / `rustc` 不在 PATH，也没有 `~/.cargo`） |
| Python | 3.10.7，`cv2` 5.0.0 可用（只影响离线跑参考测试，不影响移植） |
| 实机 | 本会话没有连接 MuMu / adb，无法做实机验证 |

M0 的第一件事是装 Rust 工具链（rustup + MSVC Build Tools + WebView2）并 `pnpm install`，
否则 `cargo test` / `pnpm tauri dev` 都跑不起来。

---

## 1. 目标与非目标

**目标**

活动终端 → 活动地图 → 扫图 → 按推荐等级选本 → 进关 → 交给 mash 现有战斗引擎 →
结算 → 回地图循环；AP 不足按配置吃苹果或等待；全程不花圣晶石。

**非目标（本阶段不做）**

- 不重写战斗、编队、助战、AP 回复、结算：全部复用 mash 现有 Runner。
- 不做每日任务 / 周常规划 / 掉落目标（P2，M5 可选）。
- 不做活动商店兑换、活动任务页（fgo-auto 自己也没做完）。
- 不改 mash 的 UI 风格，只新增一个「活动」页。

---

## 2. 三个关键决策（需要确认）

### 决策 A：EventFarm 作为现有 `Runner` 内的一种运行模式，而不是第二个 runner 进程

交接文档写的是「新增一个 event_runner」，但 mash 的设备租约
（`DeviceOperationCoordinator::reserve`）是排他的：一个进程同时只能有一个自动化持有
ADB / sidecar / 视频流。如果 EventFarm 是独立 runner，进关后要么释放租约再启动第二个
自动化（前端要接力，状态会断），要么内部再建一个 `Runner`（租约冲突）。

因此建议沿用 **rank-up quest 的模式**：在现有 `Runner` 里挂一个
`EventFarmRuntime`（`RunConfig.event_farm` 注入），主循环按画面分派：

- `EventMap` / `Terminal` / `NodeDetail` / `QuestConfirm` → EventFarm 处理器；
- `SupportSelect` / `TeamConfirm` / `Battle` / `Attack` / `BattleResult*` → 现有处理器；
- 结算完成后，普通模式照旧 `Finished`；活动模式点「下步/停止」回地图，继续下一轮。

`runner/event/` 仍然是独立模块，只是运行在同一 `Runner` 与同一设备租约里。
这样只有一套自动化、一套诊断、一套事件流。

### 决策 B：画面识别用「模板 + OCR 关键词」混合，给 sidecar 的 `detect` 增加 `keywords`

现状（已核实）：

- mash 的 `_detect_screen` **只支持模板**（`template` / `templateOptions` / `requiredTemplates`），
  没有 OCR 关键词；
- fgo-auto 的 37 个状态**绝大多数靠 OCR 关键词**，并且顺序本身是安全规则
  （例如 `gate/daily` 必须先于 `node_detail`，`defeat` 必须先于 `result`）。

如果全部改成模板，需要为 30 多个状态各裁一张图，换 UI / 换分辨率就失效，而且无法表达
「同一区域出现 A 或 B 关键词」这种规则。建议：

1. 保留现有模板快探针（战斗 / 结算 / 加载），并让**模板命中时短路返回**，
   与 fgo-auto 的「先跑便宜探针」一致；
2. 模板都没命中时，才跑关键词分类：给 `sidecar/cv-code/mash_cv/cv.py` 的
   `_detect_screen` 增加 `detect.keywords`（在 `detect.region` 内 OCR，命中任一关键词即候选），
   复用现有 `priority` 排序（**数值大者优先**，现有条目的最大值是 10）；
3. 同一帧的 OCR 结果按区域缓存，避免 30 个状态各 OCR 一次；
4. 只在 `cn/cv.json` 增加活动相关条目，不动 `shared/` 的通用条目。

这是对 sidecar 核心识别器的小幅扩展，但影响面可控，且有 `test_host_boundary.py` 兜底。

### 决策 C：双指捏合走 scrcpy 多点触控，ADB `sendevent` 只作兜底

mash 目前四层都只有单指（已核实）：

- `TouchBackend` trait 只有 `tap` / `swipe`；
- `ScrcpyTouchEvent` 没有 pointer id，Python `touch_packet` 写死 pointer 0，
  `validate_events` 要求「一个 DOWN、一个 UP」；
- ADB 后端只有 `input tap/swipe/motionevent`。

建议优先扩 scrcpy 控制通道（协议本身支持多 pointer），一次改动贯通 Rust 事件结构、
Python 编码与校验；ADB `sendevent` MT-B 作为备用后端，仅当 scrcpy 控制不可用时启用。
`touch.py` 的坐标换算是设备相关的（面板 1080×1920、`rotation=1`、
`native_x = 1080 - display_y`、`native_y = display_x`），必须在 MuMu 上重新校准，
不能直接抄进 Rust。

---

## 3. 文件清单

### 3.1 Rust（`src-tauri/src/`）

| 文件 | 改动 | 职责 |
| --- | --- | --- |
| `screen/types.rs` | 修改 | 新增 `Screen` 变体、`Display`/`FromStr`、`EVENT_DETECTION_SCREENS`、round-trip 测试 |
| `screen/client.rs` | 修改 | `use_battle_detection()` → 可切换的检测范围（Battle / Event） |
| `screen/input.rs` | 修改 | `ScrcpyTouchEvent` 增加 `pointerId`，支持多点序列 |
| `screen/protocol.rs` | 可能修改 | 若关键词检测走新 op 则登记；若并入 `detect` 则不动 |
| `runner/config.rs` | 修改 | 新增 `EventFarmConfig`（`RunConfig.event_farm`，`#[serde(skip)]` 注入） |
| `runner/mod.rs` | 修改 | `Runner` 持有 `EventFarmRuntime`，与 `RankUpQuestRuntime` 并列 |
| `runner/engine.rs` | 修改 | 按模式设置检测范围；分派活动画面；活动模式下结算后回地图而不是 `Finished` |
| `runner/results.rs` | 修改 | 活动模式的 `BattleResultContinue` 分支：回地图 / 继续下一轮 |
| `runner/ap_recovery.rs` | 修改 | 活动模式复用；苹果顺序、`max_total`、等回复预算接入 |
| `runner/event/mod.rs` | 新增 | `EventFarmRuntime` 结构、状态与阶段 |
| `runner/event/engine.rs` | 新增 | 活动主循环 / 每轮决策 / 续跑 |
| `runner/event/map.rs` | 新增 | 标签、拖动、边界、归位、缩放、二维走图、开卡 |
| `runner/event/scout.rs` | 新增 | 全量扫、增量扫、合并、缓存 |
| `runner/event/card.rs` | 新增 | 关卡卡 OCR 解析、点数、等级（含 `+` 后缀）、`is_real_quest` |
| `runner/event/decision.rs` | 新增 | `plan_once`、内存黑名单、退避、等 AP 预算 |
| `runner/event/terminal.rs` | 新增 | 活动横幅定位、弹窗关闭、回终端 / 回地图 |
| `runner/event/scout_cache.rs` | 新增 | 扫图缓存读写（应用数据目录、3 小时 TTL） |
| `runner/event/ocr.rs` | 新增 | 通用 OCR 点击助手（精确优先、最短包含） |
| `runner/event/enter.rs` | 新增 | 卡片 → ▶ → 确认 → 交给现有 Runner |
| `runner/event/story.rs` | 新增 | 剧情跳过、分支选择 |
| `runner/event/common.rs` | 新增 | 标题 / 公告 / 健康忠告 / 指引 / 从者介绍 / 背包满 / tip / busy / cg / loading |
| `runner/event/watchdog.rs` | 新增 | `cg` 150s、`unknown` 150s、`loading` 480s；强杀重启 |
| `runner/event/coords.rs` | 新增 | fgo 的 1920×1080 坐标 → `Point` / `NormRect` |
| `touch/mod.rs` | 修改 | `TouchBackend` 增加 `pinch(center, from_gap, to_gap, steps)` |
| `touch/scrcpy.rs` | 修改 | 生成双 pointer 的 DOWN/MOVE/UP 序列 |
| `touch/adb_input.rs` | 修改 | 兜底：MT-B `sendevent`（或明确返回不支持） |
| `commands/automation.rs` | 修改 | 新增 `start_event_farm_automation` / 状态查询；复用 `startup.rs` |
| `commands/automation/startup.rs` | 可能修改 | 复用会话准备；确认检测范围 |
| `device_operation.rs` | 修改 | `AutomationKind::EventFarm` + `label()` + 枚举测试 |
| `lib.rs` | 修改 | 管理状态、注册命令 |
| `runner/runtime.rs` | 修改 | 活动进度事件（复用 `AutomationEvent` + 新的 `event-farm-progress`） |

### 3.2 Python sidecar（`sidecar/cv-code/`）

| 文件 | 改动 | 职责 |
| --- | --- | --- |
| `mash_cv/cv.py` | 修改 | `_detect_screen` 支持 `detect.keywords`（区域 OCR、任一命中、按 priority 排序、帧内缓存） |
| `mash_cv/cv.py` | 修改 | 新增 `frame_signature` op：返回 240×135 灰度 PNG 的 base64，Rust 用现有 `base64` + `png` 依赖解码后算均值差，判断地图是否拖到边界 |
| `mash_cv/cv.py` | 可能修改 | `ocr_labels`（地图带批量 OCR，返回 text + box）；若 `ocr_region` 已够用则不加 |
| `mash_cv/scrcpy_control.py` | 修改 | `touch_packet` / `validate_events` 支持多 pointer |
| `tests/test_event_detect.py` | 新增 | 关键词检测单测 + corpus 回归（corpus 不存在时 skip） |
| `tests/test_data/` | 新增 | 小尺寸夹具，不提交 108 MB 语料 |

### 3.3 资源（`src-tauri/resources/servers/cn/`）

| 路径 | 改动 | 说明 |
| --- | --- | --- |
| `cv.json` | 修改 | 新增活动画面条目：`detect.keywords` / `detect.template`、`region`、`threshold`、`priority`、`variants.main` |
| `templates/event/` | 新增 | 稳定按钮模板：管理室、通知、关闭、开始任务、跳过、下步、▶、活动报酬；每个都要正样本 + 负样本 |
| `templates/event/NOTICE.md` | 新增 | 素材来源与许可（参考 `templates/NOTICE.md`） |

模板缺失或损坏时按 mash 约定**直接报错**，不做坐标兜底。

### 3.4 前端（`src/`）

| 文件 | 改动 | 职责 |
| --- | --- | --- |
| `App.tsx` | 修改 | `View` 增加 `eventFarm`、渲染分支、打开处理 |
| `featureToggles.ts` | 修改 | `eventFarm` 开关 |
| `features/home/HomePage.tsx` | 修改 | 战斗菜单增加「活动」入口 |
| `features/event-farm/EventFarmPage.tsx` | 新增 | 活动名、最多打几个本、苹果策略、扫图缓存、开始/停止 |
| `features/event-farm/EventFarmPage.test.tsx` | 新增 | 页面交互测试 |
| `types/eventFarm.ts` | 新增 | 启动请求 / 进度事件 DTO |
| `types/automation.ts` | 可能修改 | 若复用现有状态类型则不动 |

### 3.5 测试与文档

| 路径 | 内容 |
| --- | --- |
| `src-tauri/src/screen/tests.rs` | 新标签的 `Display`/`FromStr` round-trip |
| `runner/event/*` 单测 | 卡片解析（用 `labels.json` 的 6 条）、`plan_once`、`delta_targets`/`merge_delta`、退避 |
| `sidecar/cv-code/tests/test_event_detect.py` | 关键词检测；corpus 回归按 `labels.json` 逐张断言 |
| `docs/state-machines/event-farm.md` | 新增状态机（按 `docs/state-machines/README.md` 的约定） |
| `docs/state-machines/README.md` | 登记新状态机 |
| `docs/module-structure.md` | 登记 `runner/event/` |
| `THIRD_PARTY_NOTICES` | 若引入 fgo-auto 模板，补声明（`templates/NOTICE.md` 已说明部分来自 mash-app） |

---

## 4. Screen 枚举与 `cv.json` 新增项

### 4.1 现有枚举（0.27.0）

`RankUpQuest`、`TeamConfirm`、`TeamChange`、`SupportSelect`、`ServantSelect`、`Battle`、
`Attack`、`BattleResultLoot`、`BattleResultExp`、`BattleResultBond`、`BattleResultContinue`、
`BattleResultFriendRequest`、`APRecovery`、`Unknown`。

### 4.2 新增变体（分阶段）

| fgo-auto 状态 | 新 `Screen` | 检测方式 | 优先级 | 阶段 |
| --- | --- | --- | --- | --- |
| `event_map` | `EventMap` | OCR `管理室`（左上） | 60 | M0 |
| `terminal` | `Terminal` | OCR `通知`（左上） | 55 | M0 |
| `node_detail` | `NodeDetail` | OCR `关闭`（左上）+ 卡片关键词 | 30 | M0 |
| `quest_confirm` | `QuestConfirm` | OCR `是否开始任务` / `开始关卡` / `该任务没有战斗` / `败北` | 50 | M0 |
| `story` | `Story` | OCR `跳过`（右上） | 45 | M0 |
| `skip_confirm` | `SkipConfirm` | OCR `跳过` + `是否跳过` | 46 | M0 |
| `tap_continue` | `TapContinue` | OCR `请点击游戏界面` | 35 | M0 |
| `loading` | `Loading` | 模板 `tips` / OCR `加载中` / 整屏亮度 | 20 | M0 |
| `title` | `Title` | OCR `版本号`（左下） | 70 | M0 |
| `defeat` | `Defeat` | OCR `战斗失败` / `败北` / `使用圣晶石继续` / `撤退` | 80 | M0 |
| `busy` | `Busy` | OCR `该操作无效` | 15 | M0 |
| `cg` | `Cg` | 无文字 + 亮度 45~225 | 10 | M0 |
| `reward_page` | `RewardPage` | OCR `点数报酬` / `道具兑换` | 40 | M1 |
| `reward_select` | `RewardSelect` | OCR `活动奖励画面选择` | 75 | M1 |
| `gate` | `Gate` | OCR `迦勒底之门` + `关闭` | 65 | M1 |
| `servant_full` | `ServantFull` | OCR `从者数已超上限` | 78 | M1 |
| `party_confirm` | `PartyConfirm` | OCR `冠位编队确认` / `助战编队确认` | 44 | M1 |
| `repeat_confirm` | `RepeatConfirm` | OCR `连续进行同一关卡` | 42 | M1 |
| `tip` | `Tip` | OCR `该支援角色不存在` / `小贴士` | 48 | M1 |
| `list_update` | `ListUpdate` | OCR `将更新` + `成员列表` | 43 | M1 |
| `notice` | `Notice` | OCR `游戏公告` | 72 | M2 |
| `health` | `Health` | OCR `健康游戏忠告` / `抵制不良游戏` | 71 | M2 |
| `guide` | `Guide` | OCR `游玩指引` / `本月不再提示` | 70 | M2 |
| `servant_intro` | `ServantIntro` | OCR `职阶：` | 47 | M2 |
| `daily` | `Daily` | OCR `每日任务` + `关闭` | 65 | P2 |
| `support_filter` | `SupportFilter` | OCR `全部筛选设定` | 41 | P2 |
| `auto_party` | `AutoParty` | OCR `自动编成`（标题带） | 41 | P2 |
| `abort_confirm` | `AbortConfirm` | OCR `关卡中断` / `是否确定` | 76 | P2 |

**顺序是安全规则，不是美学**：`RewardSelect` > `Terminal`、`Defeat` > `BattleResult*`、
`Gate`/`Daily` > `NodeDetail`、`APRecovery` > `EventMap`、`ListUpdate` > `SupportSelect`。
`cv.json` 的 `priority` 必须按这个表落实，且每个条目都要有负样本验证。

### 4.3 `cv.json` 条目形状（示意）

```json
"EventMap": {
  "detect": {
    "keywords": ["管理室"],
    "region": { "x": 0.0, "y": 0.0, "w": 0.24, "h": 0.14 },
    "minScore": 0.5,
    "priority": 60
  },
  "variants": {
    "main": {
      "elements": {
        "reward": { "template": "event/button_reward", "region": { "x": 0.86, "y": 0.0, "w": 0.14, "h": 0.09 }, "threshold": 0.85 },
        "points":  { "keywords": ["点数"], "region": { "x": 0.03, "y": 0.16, "w": 0.22, "h": 0.30 }, "threshold": 0.8 }
      }
    }
  }
}
```

### 4.4 检测范围

`engine.rs` 目前写死 `use_battle_detection()`，只识别 `BATTLE_DETECTION_SCREENS`。
活动模式必须切换到 `EVENT_DETECTION_SCREENS`（活动画面 + 现有战斗画面），否则地图/终端
永远返回 `Unknown`。建议把 `SidecarClient` 的检测范围改成
`use_detection_scope(DetectionScope::{Battle, Event})`，普通模式行为不变。

`_detect_screen` 的求值顺序也要明确：**模板探针命中即返回**，否则再按 `priority`
排序关键词候选。否则 `EventMap`（priority 60）会在战斗帧上盖过现有 `Battle`
（priority 10），与 fgo-auto 的「便宜探针优先」相反。

---

## 5. 逐函数映射表

### 5.1 `nav.py` → Rust

| Python | 行 | Rust | 说明 / 取舍 |
| --- | --- | --- | --- |
| 坐标常量（`SKIP`…`PARTY_START`） | 41-85 | `event/coords.rs` | 全部 ÷1920/1080 → `Point`/`NormRect` |
| `watch` | 96-125 | `event/watchdog.rs` | `cg`/`unknown` 150s，`loading` 480s；超时截图 + OCR + 重启 |
| `in_fgo` / `restart_game` | 128-160 | `event/watchdog.rs` + `adb` | 查不到前台时按「在」处理；`force-stop` → `am start` → 40 次探测 |
| `set_activity` / `set_support` / `set_party` | 174-199 | `event/config.rs` | 助战/编队设置映射到 mash Runner，不在这里选助战 |
| `support_tab_point` | 210-213 | 不移植 | 由 mash 助战 Runner 负责 |
| `find_event_entry` | 216-254 | `event/terminal.rs` | OCR 右侧横幅带；活动名优先，否则按「举办时间」上方取最近 |
| `close_popup` | 263-276 | `event/terminal.rs` | 只点「关闭」，绝不点「前往」 |
| `_is_label` / `find_labels` / `labels_now` | 286-295 / 825-835 / 972-982 | `event/map.rs` | 3~10 个纯中文；离边 40px 丢弃；卡片词黑名单 |
| `_common` | 313-418 | `event/common.rs` | 逐状态映射；`abort_confirm` 选「否」；`servant_full` 只点「关闭」 |
| `handle_party_confirm` | 421-445 | `event/enter.rs` | 默认避开，点左上「关闭」 |
| `pick_choice` / `skip_story` | 452-498 | `event/story.rs` | 跳过 + 分支选最上面 |
| `collect_result` | 504-572 | 现有 `runner/results.rs` | 不重复实现；活动模式只需结算后回地图 |
| `close_servant_full` | 584-593 | `event/common.rs` | 只点「关闭」 |
| `find_entry` / `open_daily_quest` | 596-700 | P2 `event/daily.rs` | 二期 |
| `to_terminal` | 703-721 | `event/terminal.rs` | 一路返回；`result`/`battle` 交给现有 Runner |
| `to_event_map` | 724-821 | `event/terminal.rs` | 终端 → 横幅 → 地图；处理弹窗/奖励页/误入 gate |
| `_parse_card` | 842-869 | `event/card.rs` | 名称/等级/`plus`/AP/锁定/条件；`90++` 必须保留后缀 |
| `level_key` / `level_text` | 872-879 | `event/card.rs` | 排序键 `(level, plus)` |
| `is_real_quest` | 882-892 | `event/card.rs` | AP0 的区域信息卡要排除 |
| `read_card` | 895-908 | `event/card.rs` | 3 次重读，0.6s 间隔 |
| `read_points` | 911-937 | `event/card.rs` | 固定框 + 左侧大范围兜底 |
| `card_threshold` | 940-945 | `event/card.rs` | `点数达 N 以上` |
| `close_card` | 948-955 | `event/card.rs` | 有「关闭」点关闭，否则点空地 |
| `map_pan` / `map_move` | 985-1013 | `event/map.rs` | 缩略图均值差 <3.0 判边界；每次拖动后重判 `EventMap` |
| `map_to_edge` / `map_rewind` | 1015-1034 | `event/map.rs` | 最多 12 次；先 x 后 y 归位 |
| `zoom_map_out` | 1037-1088 | `event/map.rs` | 每轮 2 次捏合、最多 4 轮；每次捏合后归位；≥7 标签停 |
| `walk_map` | 1091-1127 | `event/map.rs` | 兜底 8×6 |
| `tap_label` | 1130-1141 | `event/map.rs` | 首次常被吞，重试 2 次 |
| `open_card` | 1153-1269 | `event/map.rs` | `hint_xy` 快路径 → 一屏按名找 → 二维走图 |
| `tap_text` | 1273-1291 | `event/ocr.rs` | 先精确匹配，再取包含关键词的最短文本 |
| `start_quest` | 1294-1469 | `event/enter.rs` | 卡片 → ▶ → 确认；到 `SupportSelect` 即交给现有 Runner |
| `_pick_support` | 1472-1510 | 不移植 | mash 助战 Runner 更强 |
| `run_quest` | 1518-1586 | `event/engine.rs` | 结果码 → 决策；`defeat` 只停不点 |

### 5.2 `scout.py` → Rust

| Python | 行 | Rust | 说明 |
| --- | --- | --- | --- |
| `_ensure_map` | 45-54 | `event/map.rs` | 不在图就回去 |
| `_read_labels_here` / `_visit_labels` | 57-82 / 98-135 | `event/scout.rs` | 逐标签开卡读字段；记录 `label` + `label_xy` |
| `_fresh` | 85-95 | `event/map.rs` | 每次重新取标签坐标 |
| `scan` | 138-175 | `event/scout.rs` | 最小缩放 + 归位 → 8 标签；<3 标签退回 `walk_map` |
| `delta_targets` / `merge_delta` | 179-210 | `event/scout.rs` | 只重读新标签 + 上次锁定的卡；同名保留未锁定 |
| `scan_delta` | 213-239 | `event/scout.rs` | 主线推进后增量扫 |
| `save` / `load` / `load_fresh` | 242-272 | `event/scout_cache.rs` | 应用数据目录；3 小时 TTL；缺 `plus` 字段视为旧缓存 |
| `best_free_quest` | 275-289 | `event/decision.rs` | 排除 `高难度`，按 `(level, plus, ap)` 降序 |

### 5.3 `flow.py` → Rust

| Python | 行 | Rust | 说明 |
| --- | --- | --- | --- |
| `read_ap` | 45-52 | `runner/ap_recovery.rs` | 复用 mash 的 AP 读取 |
| `is_story` / `is_farm` | 56-73 | `event/decision.rs` | AP≤10 主线，AP≥20 刷点；排除高难度 |
| `bad_key` / `load_bad` / `bump_bad` / `blacklist` | 76-112 | `event/decision.rs` | 只活在本次运行；`BAD_LIMIT=2`；绝不落盘 |
| `plan_once` | 115-156 | `event/decision.rs` | 锁主线攒点 → 推主线 → 刷最高级本 |
| `_refill_rows` / `_scroll_refill` / `_pick_apple` | 180-229 | `runner/ap_recovery.rs` | 复用并接入苹果顺序/库存 |
| `_panel_ap` / `try_apple` | 232-304 | `runner/ap_recovery.rs` | 最多 12 次；`max_total`；够了关面板 |
| `resume_in_quest` | 313-352 | `event/engine.rs` | 半路接手要计入 `max_quests`；结算页只收尾不进本 |
| `wait_ap_seconds` / `_wait_ap` | 359-393 | `event/decision.rs` | 5 分钟 1 点；累计预算；可被停止 |
| `backoff_seconds` / `_backoff` | 402-429 | `event/decision.rs` | 连续 3 次后 5/10/20/40/60s |
| `_apply_settings` | 433-476 | `event/config.rs` | 活动名/助战/编队映射 |
| `run` | 479-715 | `event/engine.rs` | 主循环、结果码分支、掉落累计 |
| `run_daily` | 718-864 | P2 `event/daily.rs` | 二期 |

### 5.4 `screens.py` / `touch.py` / `vision.py`

| Python | 去向 |
| --- | --- |
| `screens.detect` 的 37 个状态 | 第 4 节的 `Screen` + `cv.json`；顺序即 `priority` |
| `bbc_templates` 快探针 | 复用 mash 现有 `battle/` 模板；缺的补到 `templates/event/` |
| `touch.pinch` / `zoom_out` | `touch/mod.rs` + `touch/scrcpy.rs`；坐标换算在 MuMu 上重校准 |
| `vision.read` / `read_line` / `number` | sidecar `ocr_region` / `read_level_digits` |
| `vision._to_bgr` 的中文路径处理 | sidecar 已用 `read_bytes + imdecode`，沿用 |
| `vision` 的 OCR 并发 | sidecar 单实例；全流程 OCR 并发 ≤2，最好 1 |

---

## 6. 里程碑与验收

| 阶段 | 内容 | 验收 |
| --- | --- | --- |
| **M0**（已完成 2026-11-02） | 装工具链；新增 M0 的 `Screen` + `cv.json` + 关键词检测；corpus 离线回归 | `cargo test` / `pnpm build` 通过；已实现画面的 corpus 正/负样本全通过（见第 9 节） |
| **M1**（代码完成 2026-11-02，待实机验收） | 捏合、拖动、边界、归位、标签 OCR、关卡卡解析、扫图缓存 | 实机扫出 8 个标签；卡片名称/等级/AP/锁定与手工核对一致；缓存 3 小时生效 |
| **M2**（代码完成 2026-11-03，待实机验收） | 选本 + 进关：点标签 → 开卡 → ▶ → 确认 → 交给现有 Runner | 从地图进入战斗，全程无人工；`SupportSelect` 起由 mash 接管 |
| **M3** | 循环：结算 → 回地图 → 下一轮；失败/掉线恢复 | 连续打 5 个本不中断；全灭只停不复活 |
| **M4** | AP 检查、苹果、等回复、掉落目标 | 配置生效；圣晶石永远不在默认顺序；`max_total` 与等待预算正确 |
| **M5** | 每日任务 / 周常（可选） | 按档位/篇选本，不看 AP |
| **M6** | 打包 / 诊断包 / 文档 | Windows 安装包可跑；失败现场可导出 |

每阶段先写测试（Rust 单测 + sidecar pytest + corpus 离线），再连实机。

---

## 7. 风险清单

| # | 风险 | 影响 | 应对 |
| --- | --- | --- | --- |
| 1 | **Rust 工具链未安装**（已核实） | M0 完全阻塞 | 先装 rustup + MSVC Build Tools + WebView2；`pnpm install` |
| 2 | **mash 四层都没有多点触控** | 地图无法缩到最小，扫图退化成 8×6 拖图 | 按决策 C 改 scrcpy 多点；实机校准；保留 `sendevent` 兜底 |
| 3 | **检测范式不一致**：mash 只认模板，fgo 靠 OCR 关键词 + 顺序 | 直接照搬会识别不出地图/终端/卡片 | 按决策 B 给 `detect` 加 `keywords`；保留模板快探针 |
| 4 | **检测范围写死战斗画面** | 活动画面永远 `Unknown` | 增加 `DetectionScope::Event`，普通模式不变 |
| 5 | **Runner 结算后 `Finished`** | 活动模式打完一把就退出 | 活动模式改回地图并继续；普通模式行为不变 |
| 6 | **画面优先级写错会花资源** | 误点进本、误用圣晶石、误动从者 | 按第 4 节 priority；每个高风险画面配负样本；`defeat`/`gate`/`servant_full` 只读不点 |
| 7 | **坐标写死 1920×1080** | 换分辨率/模拟器全错 | 全部 `NormRect`；捏合坐标按设备面板重算 |
| 8 | **OCR 性能** | 扫图慢、机器卡死 | ROI 优先、帧内缓存、并发 ≤2；全屏 OCR 只在兜底用 |
| 9 | **corpus 108 MB 不能进仓库** | 回归测试无法在 CI 跑 | 只提交 `labels.json`（4 KB）；corpus 缺失时 skip；本地回归 |
| 10 | **换期活动** | 横幅/地图/活动名全变 | 活动名配置 + `preflight` 自检 + 清缓存入口；扫图结果 3 小时过期 |
| 11 | **掉线/崩溃** | 一轮白打 | 看门狗 + 重启；掉线自恢复在 fgo-auto 里也未实测，按「未验证」对待 |
| 12 | **中文路径 / adb 子进程** | 截图静默失败、黑框 | sidecar 已处理；Rust 侧 adb 沿用现有封装 |
| 13 | **设备互斥** | 两个自动化同时点游戏 | `AutomationKind::EventFarm` 走 `reserve`；单实例 |
| 14 | **只支持 Windows + MuMu + 国服** | 其他环境不保证 | 启动时校验分辨率/包名；失败直接报错 |
| 15 | **本会话没有实机** | 无法验证真机行为 | M0 只做离线；M1 起需要用户配合上机 |

---

## 8. 需要你提供 / 确认

1. **安装 Rust 工具链**：是否同意现在装 rustup + MSVC Build Tools + WebView2？没有它 M0 无法开始。
2. **检测方案**：同意「模板 + OCR 关键词」混合（决策 B），还是坚持全部用模板？
3. **多点触控**：优先 scrcpy 多点，ADB `sendevent` 兜底（决策 C）是否可接受？
4. **交接边界**：活动 Runner 到 `SupportSelect` 就把控制权交给现有 Runner（决策 A），
   还是希望活动 Runner 一直管到战斗结束？
5. **实机素材**（换期活动后最好重新提供）：
   - 终端活动横幅（含「举办时间」）
   - 活动地图（缩到最小 + 归位后的一屏）
   - 关卡卡：已解锁 / 锁定 / `90++` 各一张
   - 任务确认框、AP 回复面板、全灭失败页、编队确认页、点数报酬页、剧情跳过页
   - 启动链：标题 / 公告 / 健康忠告 / 游玩指引 / 从者介绍
6. **UI 范围**：活动页只做「活动名 + 最多打几个 + 苹果策略 + 扫图缓存 + 开始/停止」够不够？
7. **分支**：直接在 `main` 上做，还是开 `feat/event-farm`？

确认后从 M0 开始：先装工具链、跑通现有测试，再加 `Screen`/`cv.json`/关键词检测和 corpus 回归。

---

## 9. M0 落地记录（2026-11-02）

已完成：

- Rust 工具链（rustup + MSVC）与 pnpm 依赖装好，`cargo test` 可跑。
- `Screen` 新增 12 个活动画面；`EVENT_ONLY_DETECTION_SCREENS` 与
  `SidecarClient::use_event_detection()` 定义“战斗 + 活动”识别范围。
- sidecar 识别改为三段式：模板 → OCR 关键词 → 帧亮度，先命中先返回；
  `keywords`（任一）/`keywordGroups`（合取）/`minScore`/`blank` 四种配置形状。
- `cn/cv.json` 加入 M0 的 12 个画面条目及优先级；`docs/state-machines/event-farm.md` 记录路由。
- 合成用例 10 条 + 真实语料回归 20 条（`reference/` 缺失时自动跳过）。

与计划的偏差（以 `docs/state-machines/event-farm.md` 为准）：

1. `Cg` 不设探针。全屏 CG 没有可读文字，亮度区间又和正常画面重叠，直接观测为
   `Unknown`；事件循环对 `Unknown` 与 `Cg` 的处理都是“等待”，行为一致。亮度探针只用于
   `Loading` 的纯黑帧（`blank.maxBrightness`）。
2. `NodeDetail` 用“左上 `关闭` + 卡片关键词”的合取，而不是只看 `关闭`，否则会在
   `gate`/`daily` 页面上误判并在自由本列表里盲点。
3. 战斗 runner 把活动画面并入 `Unknown` 分支（同一 `match` arm），而不是额外包一层
   映射函数：语义相同，少一层间接，编译期仍保持穷尽匹配。
4. JP 暂不配置活动画面；活动模式先只支持 CN。

环境说明：本机（Python 3.10 + OpenCV 5.0）跑 `tests/test_cv.py` 时有 19 条既有的 OCR/
数字识别用例失败（助战分数、羁绊升级名字截断等）。用 `git stash` 回到改动前的 `cv.py`
复跑，失败集合完全一致，属于环境差异而非本次改动；屏幕识别相关用例全部通过。

corpus 验收口径：M0 只断言已实现的 18 张正样本截图（event_map / terminal / node_detail /
quest_confirm / story / skip_confirm / tap_continue / loading）与 battle/result 负样本；
其余标注图对应的画面（support、party、reward_page、gate、daily 等）在各自里程碑落地时
再加入断言。`59_connecting.png`（白底「连接中」）是已知缺口，见状态机文档。

M0 验证结果（本机）：

- `cargo test --manifest-path src-tauri/Cargo.toml`：888 passed / 0 failed。
- `sidecar/cv-code` 事件用例：合成 11 passed；corpus 20 passed（18 正样本 + 1 负样本组 + 1 优先级）。
- `pnpm build`：eslint / tsc / vitest 735 passed / vite build 全部通过。
- 未执行 `pnpm tauri dev`：会打开 GUI 窗口，留待实机联调时运行。

---

## 10. M1 落地记录（2026-11-02）

已完成：

- `Screen` 再补 8 个画面：`RewardSelect`、`RewardPage`、`ServantFull`、`Gate`、`Tip`、
  `PartyConfirm`、`RepeatConfirm`、`ListUpdate`，共 20 个活动画面。
- 双指缩放打通四层：`TouchBackend::pinch`（gap 为两指总间距）、`HumanizedTouch` 抖动、
  scrcpy 多点控制包（`pointer_id`）、ADB `sendevent` MT-B 兜底；侧车
  `validate_events` 按指针追踪按下状态。
- sidecar 新增 `read_map_labels` / `read_quest_card` / `read_event_points` /
  `frame_thumbnail`，逻辑集中在 `mash_cv/event_map.py`。
- `runner/event/`：`card`（关卡卡解析与最高等级选本）、`map`（平移/边界/归位/捏合）、
  `scout`（增量目标与合并）、`cache`（3 小时 TTL）、`scan`（扫图编排）。
- 调试命令 `debug_scan_event_map` + 前端「扫描活动地图」按钮，用于实机验收。

与计划的偏差（以 `docs/state-machines/event-farm.md` 为准）：

1. 捏合参数用**两指总间距**，参考实现的 300→60 写作 600→120，避免半间距语义在下层
   重复换算。
2. 扫图只实现「缩到最小后一屏放下」这一种：标签少于 3 个直接报错，二维拖图兜底留到
   M2 与导航一起做，而不是在半张地图上猜坐标。
3. 边界判定用 64×36 缩略图帧差（< 3.0），不比对地图坐标：地图边缘是空地，帧差更可靠。
4. 新增 `overrides` 配置：确认框复用底页模板（`ListUpdate` 被 `SupportSelect` 模板命中），
   模板命中后只对声明了覆盖的画面再跑一次关键词段。
5. `PartyConfirm` 的探针框从中间标题区移到右上标题带：OCR 在中间区把标题切成
   `冠位编` + `确认`，只有右上带能读到完整的 `冠位编队`。
6. `Gate` 关键词补短形式（`迦勒底之` / `勒底之`）：OCR 常把 `迦勒底之门` 切成
   `迦车` + `勒底之`，整句匹配永远不中。

已知限制：ADB 捏合假设 1080×1920 / rotation-1 / `event4`；`每日任务` 仍观测为
`NodeDetail`；白底 `连接中` 仍为 `Unknown`。

M1 验证结果（本机）：

- `cargo test --manifest-path src-tauri/Cargo.toml`：912 passed / 0 failed / 10 ignored。
- sidecar：事件相关合成用例全通过；corpus 28 passed（26 张正样本 + 1 组战斗/结算负样本 +
  1 条优先级顺序）。
- `pnpm build`：通过（含 DebugPage 扫描按钮用例）。
- 其余既有 pytest 失败与本机环境有关，改动前即存在：`test_cv.py` 19 条（OCR/数字模板与
  OpenCV 版本差异）、`test_ce_main_target.py` 2 条（1920/2560 下的等级模板）、
  `test_verify_code_release.py` 7 条与 `test_build_code.py` 1 条（需要 Python 3.11 的
  `hashlib.file_digest` / `tomllib`，本机是 3.10）。
- 实机验收未做：需要用户在地图页点「扫描活动地图」核对 8 个标签与卡片数值。

---

## 11. M2 落地记录（2026-11-03）

已完成：

- `EventFarmConfig`（`target_quest`）挂在 `RunConfig.event_farm`；启动命令
  `start_event_farm_automation` 复用战斗启动链，设备租约新增
  `AutomationKind::EventFarm`，仅国服可用。
- `Runner` 挂载 `EventFarmRuntime`（决策 A，与 `RankUpQuestRuntime` 并列）；识别范围
  按模式切换到 `use_event_detection()`；主循环把 `EventMap` / `NodeDetail` /
  `QuestConfirm` / `Gate` 路由进 `runner/event/farm.rs`。
- 选卡：3 小时扫图缓存优先，失效现场扫描并回写；指定名按 `hint_ok` 匹配（取最高等级），
  否则 `best_farm_quest`。标签坐标每次 OCR 现找，找不到等待重试，不猜坐标。
- 进关坐标取参考实现（▶ 1370,320；开始 1259,846；卡片关闭 163,63），归一化后经
  `tap_at` 走人类化抖动；标签点击走 `tap_screen_point`（同为设备像素 + 抖动）。
- 同一标签最多 3 次尝试（卡片被点掉 / OCR 找不到标签都计入），超限报错停止。
- 结算页 `BattleResultContinue` 活动模式下点「结束」并停止；M3 再换成回地图循环。
- 入口：战斗页「活动刷本」开关（临时 UI 状态，不落项目配置），打开后启动命令切到
  `start_event_farm_automation` 并携带 `eventFarm: {}`；dev mock 与页面测试同步。

与计划的偏差：

1. 未做 `plan_once` 完整策略（主线/点数/退避/等 AP）——那是 M4；M2 的选卡就是
   「指定名或最高等级可打本」。
2. 未新增独立活动页：战斗页开关复用项目/助战/苹果配置，正式活动页按计划仍是 M6 前
   的独立条目。
3. `Story` / `Tip` / `PartyConfirm` 等进关途中可能出现的画面暂不处理（等待直至超时），
   与参考实现的跳过逻辑一起归入 M3/M4。

M2 验证结果（本机）：

- `cargo test --manifest-path src-tauri/Cargo.toml`：918 passed / 0 failed / 10 ignored。
- `pnpm build`：eslint / tsc / vitest / vite build 全部通过（含活动开关用例）。
- 实机验收未做：地图扫描（M1）+ 进关（M2）需一次上机连验。
