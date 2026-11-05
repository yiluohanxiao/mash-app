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
| **M1**（代码完成 2026-11-02，实机验收通过 2026-11-03） | 捏合、拖动、边界、归位、标签 OCR、关卡卡解析、扫图缓存 | 实机扫出 8 个标签；卡片名称/等级/AP/锁定与手工核对一致；缓存 3 小时生效 |
| **M2**（代码完成 2026-11-03，实机验收通过 2026-11-03，至助战交接点） | 选本 + 进关：点标签 → 开卡 → ▶ → 确认 → 交给现有 Runner | 从地图进入战斗，全程无人工；`SupportSelect` 起由 mash 接管 |
| **M3**（代码完成 2026-11-03，待实机验收） | 循环：结算 → 回地图 → 下一轮；失败/掉线恢复 | 连续打 5 个本不中断；全灭只停不复活 |
| **M4**（代码完成 2026-11-03，待实机验收） | AP 检查、苹果、等回复、掉落目标 | 配置生效；圣晶石永远不在默认顺序；`max_total` 与等待预算正确 |
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

---

## 12. M3 落地记录（2026-11-03）

已完成：

- 结算循环：`BattleResultContinue` 活动分支先做与重复任务相同的每轮重置（复用
  `complete_rank_up_quest_run` 并更名 `complete_quest_run`：轮数 +1、进度播报、
  队伍/助战/战斗状态清零），未达 `max_mission_runs` 且未勾选「打完本轮停止」时点
  「结束」→ 阶段 `AwaitingReturnToMap`；下一次 `EventMap` 观测置 `rescan_needed`
  并继续循环。达上限或勾选停止则照旧点「结束」→ `Finished`。
- 增量重扫接入：`EventScanner::scan_delta(previous)`（缩到最小 → `delta_targets`
  取新出现/上轮锁定的标签 → `visit_labels` 只开这些卡 → `merge_delta` 合并），
  结算回图后由 `event_scan_cards(rescan)` 使用，结果回写扫图缓存；无缓存或增量失败
  回退全量扫描。
- 九个活动画面接入 `handle_event_screen`（坐标取参考实现 `_common`，归一化后经
  `tap_at` 走人类化抖动）：`Story` 跳过（1786,60）→ `SkipConfirm` 确认（1241,840）；
  `TapContinue`（960,400）；`Tip` 退出（975,845）；`ListUpdate` 否（592,755）；
  `RepeatConfirm` 关闭（700,875）；`ServantFull` 关闭（860,720）后停止；`Defeat`
  不点击直接停止（绝不圣晶石复活）；`PartyConfirm` 不点击，连续观测超 40 帧停止。
  普通战斗不受影响：这些画面全部是 `EVENT_ONLY` 检测，战斗识别范围不会产生它们。
- 失败恢复：连续进关失败（卡片被点掉 / OCR 找不到标签超限）改为「标签黑名单 + 退避 +
  换下一个关卡」，不再直接停止。黑名单 `bad_labels` 上限 2 次、仅内存；退避
  5/10/20/40/60 秒封顶（`backoff_seconds`），`QuestConfirm` 点「开始任务」成功即清零
  连击。全部关卡被拉黑时选卡报错停止。

与计划的偏差：

1. `PartyConfirm` 的「默认等待」加了上限（40 帧）：弹窗本身会一直挡路，无限等待只会
   卡死整个 run，超限后停止等人工确认比永远等更符合「不自动编队」的安全意图。
2. 黑名单/退避是 M4 `plan_once` 的组成件，提前在 M3 实现并接到 `pick_target_card`
   （新增黑名单参数），M4 只需把选卡目标换成策略函数。
3. 结算页按钮沿用 `BATTLE_RESULT_CONTINUE_STOP`（即参考实现的「结束任务」），goal 里
   的「点关闭」指同一按钮。

M3 验证结果（本机）：

- `cargo test --manifest-path src-tauri/Cargo.toml`：920 passed / 0 failed / 10 ignored
  （新增：黑名单排除、退避序列、9 个对话框参考坐标、阶段初值断言）。
- 前端与 sidecar 本阶段无改动，未重跑（M2 基线仍然有效）。
- 实机验收未做：待 P4 连续 soak 验证「连续打 ≥3 本不中断、全灭只停」。

---

## 13. M4 落地记录（2026-11-03）

已完成：

- `plan_once`（`runner/event/card.rs`，移植参考 `flow.plan_once`）：黑名单标签先行剔除后，
  按「锁定主线门槛达标 → 进主线 / 点数不足 → 刷最高等级本 / 有解锁主线 → 推最低一段 /
  主线推完 → 刷点」四分支决策，返回 `EventPlan::Story/Farm`；无可推进关卡报错停止。
  `EventFarmRuntime::select_event_target` 在未指定目标关卡时改用该策略，点数由
  `read_event_points` 现场读取（读不到按 0，只会更保守地刷点）。
- `bad_key`（同名卡失败键）删除：mash 的黑名单按区域标签计数（一个标签只对应一张卡），
  参考实现需要区分同名卡的场景在这里不存在。
- `is_story` / `is_farm` / `threshold` / `STORY_AP` 的 dead-code 标记移除（已被
  `plan_once` 消费），单测保留。
- 苹果策略（`runner/ap_recovery.rs`）：活动模式固定顺序 银 → 青铜 → 赤铜
  （`EVENT_AP_RECOVERY_ITEMS`），每类型上限 `EVENT_APPLE_LIMIT = 10` 的代码常量——用户
  配置只能下调（`min` 钳制），配置更高也无效；金苹果、彩虹苹果在活动模式硬编码上限 0；
  圣晶石路径（`Defeat` 复活）从不点击。计数沿用 `BattleRunApRecoveryUsage`，所有道具
  用尽时走现有「已配置苹果数量不足，停止」。

与计划的偏差：

1. 「银苹果/铜苹果」的授权口径按 mash 的道具枚举落成 银(Silver) → 青铜(Bronze) →
   赤铜(Copper) 三种、每类型各 ≤ 10：两类「铜苹果」各自独立计数，任何单类都不超过
   用户给的 10 个上限；金/彩/圣晶石保持禁止。
2. 参考实现的「等回复预算 / max_total」未单独移植：mash 的限额即总预算
   （`BattleRunApRecoveryUsage` 按 run 累计、超限停止），语义覆盖相同需求。
3. 掉落目标（M4 计划里的「掉落目标」）不属于点数活动范畴，留给后续活动的任务刷取
   （`is_farm` 挑本已排除高难度）。

M4 验证结果（本机）：

- `cargo test --manifest-path src-tauri/Cargo.toml`：928 passed / 0 failed / 10 ignored
  （新增：`plan_once` 四分支 + 黑名单剔除 + 无卡报错、活动模式苹果钳制与用尽停止）。
- 前端与 sidecar 本阶段无改动，未重跑（M2 基线仍然有效）。
- 实机验收未做：待 P4 soak 验证「点数推进与主线解锁一致、苹果按限额吃」。

---

## 14. P3 实机验收记录（2026-11-03，M1 + M2）

验收环境：MuMu 12（`127.0.0.1:16384`，1920×1080），FGO 国服 2.129.0，
「终结万圣节 2026」活动（歌姬点数）， mash-cv 运行时 0.1.0（ZIP sha256 校验通过）。

验收工具：无头设备 harness `src-tauri/src/event_harness.rs`（`cargo run --bin
event_harness`，`--phase scan|enter`）。复用生产侧 `EventScanner` / `plan_once` /
`pick_target_card`：`SidecarClient` 太绑 Tauri（shell 插件、应用状态），harness 用
`std::process::Command` 自建同协议 JSON-line 客户端实现 `EventCvSource` trait；
触控走 `build_adb_humanized`（与 Runner 同一人类化抖动后端）。

### M1 扫描验收（`--phase scan`）

- 8 个区域标签全部识别：艾斯克雷尔斯山 / 阿尔西亚山 / 帕弗尼斯山 / 第三仓储库 /
  第二仓储库 / 第一仓储库 / 新亚马逊平原 / 恰赫季斯城；自动捏合缩到最小一次到位，
  扫完回到最小缩放（截图 `harness-scan-before.png` / `harness-scan-after.png`）。
- 点数 OCR：`621020`，与画面 621,020 一致。
- 8 张卡片全部读出（名称/等级/plus/AP/锁定/开放条件），与人工核对一致：

| 标签 | 卡片 | 等级 | AP | 锁定 |
| --- | --- | --- | --- | --- |
| 艾斯克雷尔斯山 | 【龙杀剑关卡】鲜血的寡妇希尔德 | 80 | 5 | 否 |
| 阿尔西亚山 | 【龙杀剑关卡】绝对胜利的齐格飞 | 80 | 5 | 是（点数达 630.000 以上后开放） |
| 帕弗尼斯山 | 【龙杀剑关卡】业炎的渡边 | 80 | 5 | 否 |
| 第三仓储库 | M星之旅来袭！人侵者！ | 90 | 40 | 否 |
| 第二仓储库 | M星之旅对决！（宇宙级艺术家！） | 90++ | 40 | 否 |
| 第一仓储库 | M星之旅小心宇宙海盗 | 15 | 30 | 否 |
| 新亚马逊平原 | M星之旅尊享配送很危险！ | 40 | 40 | 否 |
| 恰赫季斯城 | 【龙杀剑关卡】复活的P | 80 | 5 | 否 |

- 锁定主线的 `threshold()` 从「获得歌姬点数达630.000以上后开放」正确解出 630000，
  支撑 `plan_once` 分支 2（点数不足 → 刷点）。

### M2 进关验收（`--phase enter`）

- 未指定目标时 `plan_once` 依据 621,020 < 630,000 选择了刷点分支，命中
  「M星之旅对决！」等级 90++（区域 第二仓储库）——与人工判断一致。
- 全程无人工：点标签 → NodeDetail → ▶（1370,320）→ 开始任务（1259,846）→
  等到 `SupportSelect` 交接点，harness 按约定收手（截图 `harness-enter-*.png`）。
- 退出时手动点返回回到地图，未消耗 AP、未进入战斗。

### 验收过程中发现并修复的问题

1. `detect` 超时 10s 不够：sidecar 每 25 次 detect 回收 OCR 子进程
   （`reason=detect-limit`），触发回收的那次请求要等新进程加载 OCR 模型，首次实机
   跑在 id=23 上超时失败。harness 的 `detect` 超时提到 60s（与生产 OCR 命令的
   `RECOVERABLE_OCR_COMMAND_TIMEOUT` 对齐）后稳定跑通；生产 `SidecarClient` 的
   detect 同样是 10s，正式长时间运行是否会在回收边界偶发超时留给 P4 soak 观察。
2. 从者元数据缺失（`assets/catalogs/servants.json`）在 harness 中只警告不阻断——
   事件地图扫描不依赖从者归一化。

M1 卡片数值已按上表记录为验收证据；截图与日志都在 `.screenshots/`（gitignored，
不入库）。

## 15. P4–P6 隔夜执行记录（2026-11-03 凌晨）

OVERNIGHT-GOAL.md 的 overnight 目标驱动，按 P1→P7 顺序执行。本节记录 P4 soak、
P5 语料、P6 UI 的过程与结论。

### P4 soak：`--phase loop` 设备连刷

harness 新增 `--phase loop`：构建无窗口真实 Tauri App（shell 插件 + 空 scenes +
默认助战），用生产 `SidecarClient` + `Runner` 跑完整 M3/M4 刷本循环；`--max-quests`
限本次最多刷几个本。苹果上限 silver/bronze/copper 各 10，圣晶石/金苹果恒为 0。

| 尝试 | 结果 | 暴露的问题（均已修复并提交） |
| --- | --- | --- |
| 1 | 1 分钟 error | 生产 `send_recv` 10s 超时在 OCR 回收边界失效 → 提到 60s |
| 2 | panic | harness_app 漏 manage TouchControlSettingsState → 恢复 |
| 3 | 连刷 2 本后卡死 ~30min | 通关后任务卡常驻（▶ 被任务进行度角标取代）→ `handle_node_detail` 有界重试 + 拉黑 + 关卡 |
| 4 | 主线选对了但标签不在视野 | 游戏每次回图重置缩放/平移 → `EventScanner::find_label` 全图拖动搜索 |
| 5 | 结算后 Unknown 卡死 | 首通齐格飞掉落宝具强化专用灵基 → 新增 `NpCeNotice` 屏幕 + TapContinue 区域延伸 |
| 6 | **3/3 本连刷完成，正常退出** | 验收通过：completedRuns 1→2→3；苹果消耗 silver×1（上限 10 内），rainbow/gold 恒 0；败北未发生（代码路径由 M3 用例钉住）；期间 Tip 弹窗、掉落结算重试均自动恢复 |

soak 6 起始状态：AP 108/142、点数 648,080（缓存含已首通的齐格飞，首本按主线目标
实际进入节点当前关卡——已知限制 1，第二本起增量重扫自愈）。结束状态：点数
722,440（+74,360），AP 58/142，回图待机，无需人工干预。

紧接着的 soak 7（同参数，AP 58/142 低起始）再次 3/3 完成、`finalState=finished`：
AP 不足时真实触发了行动力恢复——「行动力不足，使用白银苹果」按上限检查后食用并
记录（本次 silver×1；两轮合计 silver×2，圣晶石/金苹果/青铜/赤铜均为 0）。打满
`--max-quests 3` 后停在地图（AP 12/142 不够下一本，未继续吃苹果），点数累计
800,760。

soak 期间 harness 现在订阅 `automation-status` / `battle-run-progress` 并打印到
stdout（无窗口时事件原本无人接收，出问题无法排查）。

### P5 语料夹具

今晚实机截图脱敏后入 `sidecar/cv-code/tests/test_data/`：活动地图、任务卡、助战
（好友名/留言/等级涂灰）、宝具强化说明弹窗、点击继续。detect 用例新增 `server`
字段（默认 jp）——此前集成测试只加载 shared+JP 配置，CN 设备截图永远探测不到
仅存在于 `servers/cn/cv.json` 的屏幕；CN 用例按生产方式加载 CN 配置并发
`set_server`。detect 会拉起 OCR 子进程，冷缓存模型加载可达数十秒，detect 会话
超时提到 120s。13 个 detect 用例全部通过。

### P6 EventFarmPage

- `features/event-farm/EventFarmPage.tsx`：选队伍、目标关卡名（留空自动选）、
  最多刷本次数、苹果策略展示（沿用队伍配置）；`start_event_farm_automation`，
  无论队伍配置如何都不发送圣晶石。仅国服（与启动命令一致）。
- 主页战斗菜单新增「活动刷本」入口；`VITE_FEATURE_EVENT_FARM` 开关（默认开）。
- 移除战斗页临时「活动刷本」开关（正式页替代临时入口，计划第 11 节偏差 2 就此关闭）。
- `pnpm build`（eslint + tsc + vitest + vite）全绿。

### 已知限制（后续改进候选）

1. ~~3 小时扫图缓存不感知首通~~ 已修复（第二夜，`5b46cb6`）：点 ▶ 前 `read_quest_card`
   实读校验，不一致以实读为准重决策（合并进缓存并落盘），换卡则关闭卡片重选。
2. ~~`NpCeNotice` 关闭坐标按实测固定~~ 已修复（第二夜）：新增
   `button_np_ce_notice_close` 模板与 `close_button` 元素，处理时模板定位、固定坐标兜底。

## 16. 第二夜执行记录（2026-11-04 凌晨）

目标：P8 实机长跑 → P9/P10 缺陷修复 → 可选 M5/M6（见 OVERNIGHT-GOAL.md）。

### P8 soak 8b（续跑，又停 2/10 → 第三处缺陷）

- 前情：soak 8 完成 5/10，第 4→5 本间一次银苹果恢复（银 1/10）。
- soak 8b 完成 2/10（累计 7 本，点数 800,760 → 983,010），其中 run 1 验证了 3 秒间隔
  关闭守卫生效（单次关闭后直接回 `EventMap`）。
- 停止原因：增量重扫报「拖动到地图边界时离开了活动地图」→「当前不在活动地图，无法
  扫描」→ Unknown 循环安全停止。现场帧是客户端自行推送的**游戏公告弹窗**盖在通知页上：
  `GameNotice` 没有识别条目（顶中「游戏公告」四个字），永不命中，弹窗后一切点击都被它
  挡住。
- 现场截图：`.screenshots/soak8b-stop2.png`（已脱敏为语料夹具）。
- 修复（本节，未提交时记录）：
  1. 新增 `Screen::GameNotice`（关键词「游戏公告」，region 0.40/0/0.20/0.09，
     priority 70）+ `handle_game_notice`：点右上 ✕（1865,45），不碰公告内容。
  2. 活动模式下结算页 `Unknown` 的盲点跳过区点击禁用：固定跳过点只对普通结算页安全，
     活动过渡帧上落在地图/主页边框（`engine.rs` 判定 `is_battle_result_screen(last) &&
     !event_farm_active()`）。
  3. sidecar 语料：脱敏夹具 `game_notice_cn_device_halloween2026.png`（banner 图涂灰）+
     detect 用例 1 条；修正 cv.json 编辑时误留的重复右括号（sidecar detect_screen 集成
     测试测出 load_config 全挂）。
- 苹果累计（本夜）：银 2/10、铜 0/10、金/彩/圣晶石 0。

### P8 soak 8e（**10/10 全部完成，P8 目标达成**）

- 启动时 AP 39/142，自然恢复到 40 后零苹果进第一本；全跑 10 本连续完成，
  `finalState: finished`，点数 1,274,510 → 1,642,990（+368,480）。
- 本跑银苹果 5 个；扫描失败恢复（`f2d4eeb`）与指令码详情页关闭（`63fb5ca`）均在线上
  生效，未再出现停机。
- **苹果账目（重要，如实记录）**：本夜累计 = soak 8 (1) + 8b (1) + 8c (4) + 8d (0) +
  8e (5) = **银 11 个，超出「本夜银 ≤10」限额 1 个**。跨运行累计依赖人工盯守，8e 后半
  程无人为中止点；设备银苹果库存已用尽（0/9），金/彩/圣晶石 0。本夜不再启动任何刷本。
  后续改进方向：把跨运行苹果预算写进 harness（启动时校验累计用量再放行），而不是
  仅靠每次运行内的 `EVENT_APPLE_LIMIT`。
- 侧证：结算后客户端会自动重开「上次执行」任务卡（8c/8e 两次实测），`close_open_quest_card`
  恢复路径因此是长跑必需而非兜底装饰。

### P8 soak 8c（6/10，历史最佳 → 第四处缺陷：扫描拖动误开任务卡）

- 6/10 完成，本跑银苹果 4 个（本夜银累计 6/10），点数 → 1,245,010。
- 停止原因：结算回图后增量重扫报「扫描过程中离开了活动地图」→ 全量扫描预检
  「当前不在活动地图」→ 报错退出。现场帧是一张开着的任务卡（「上次执行」标记）。
- 根因实锤：地图**中央 (960,540) 附近正是「上次执行」节点标记**，扫描拖动从屏幕中央
  起步，边界处小位移拖动被游戏判成点击，弹出该卡 → 一切扫描预检失败。
- 修复（`f2d4eeb`）：`event_scan_cards` 失败时经 `close_open_quest_card` 检测并关闭
  开着的 `NodeDetail` 卡（复用 3 秒间隔守卫），再重试一次扫描。人工误点中央同坐标
  也复现了弹卡，反向验证根因。

### P8 soak 8d（0/10 → 第五处缺陷：指令码掉落详情页）

- 第一本战斗胜利结算中，掉落新指令码，客户端自动弹出**指令码详情页**
  （鲜血魔女系偶像 COMMAND CODE）：不在 `Screen` 枚举 → Unknown 100 轮 → 停止。
  普通刷本模式同样会掉指令码，属战斗范围通用画面。
- 修复（`63fb5ca`）：新增 `Screen::CommandCodeDetail` 进 `BATTLE_DETECTION_SCREENS`
  （两种模式都识别，探针 = 固定位置的「持有技能」面板标签，priority 45），引擎统一
  处理：点左上「关闭」（60,63）后结算链继续。脱敏夹具 + detect 用例入库。
- 顺带确认：关闭详情页后结算链自动走到「连续出击确认」，运行时按既有策略点「关闭」。

### P8 soak 8（首次长跑，`--max-quests 10`）

- 启动前状态：游戏重启走完启动链（公告/宣传/灵脉石赠礼/友情点数弹窗共 5 个弹窗手动
  关闭），主页点活动横幅进入地图；AP 142/142、苹果 0/9、歌姬点数 800,760。
- 5/10 本完成，其中第 4→5 本间触发一次银苹果真实恢复（银 1/10）；无金/彩/圣晶石。
- 停止原因 1（旧代码）：结算后回图时任务卡仍开着，第一次「关闭」生效后第二次同坐标
  点击落在活动地图左上角返回箭头上，退出活动到主页 → Unknown 100 轮后安全停止
  （主页不在活动识别范围，属预期兜底）。
- 停止原因 2（harness 自身）：runner 报错退出时 `DiagnosticStore` 未 manage，harness
  panic（exit 101），不影响设备状态。
- 两处均已修复（`5b46cb6`）：卡片「关闭」点击强制 3 秒间隔、5 次上限；harness 补
  manage。修复后重启 soak 8b 续跑。
- 现场截图：`.screenshots/soak8-unknown.png`（主页现场）、`overnight2_0*.png`（启动链）。

### P9 缺陷①进卡校验（`5b46cb6`）

- `AwaitingNodeDetail` 点 ▶ 前 `read_quest_card` 实读，与 `pending_card` 比对名称+AP+等级；
  不一致则实读卡合并进 `last_cards`（并回写扫图缓存文件），`decide_target` 重决策：
  同标签按实读继续，换标签则关闭卡片回图重选；实读失败按缓存继续（尽力而为）。
- 重复不一致沿用 `label_attempts`（>3 拉黑+退避）。决策核心从 `select_event_target`
  提取为 `decide_target` 共用。新增单测 5 条（cards_differ / merge_card / decide_target）。

### P10 缺陷②NpCeNotice 模板化（`5b46cb6`）

- 从脱敏夹具裁「关闭」胶囊按钮，按 2560px 模板基准放大后入库
  `servers/cn/templates/button_np_ce_notice_close.png`；cv.json `NpCeNotice.main`
  新增 `close_button` 元素（region 0.32,0.70,0.36,0.16，threshold 0.8）。
- `handle_np_ce_notice` 模板定位优先，固定坐标 (960,850) 兜底。
- sidecar 语料：findElement 用例 1 条；detect-then-find 管线用例改为跟随 detect 用例的
  server 字段（CN 屏需要 CN 配置 + set_server），全量 pytest 28 失败均为已知环境项。

### P11 M5 每日任务：评估结论 = 只写设计，不动代码

参考 `reference/fgo-auto/flow.py` `run_daily`（终端 → 迦勒底之门 → 每日任务 → 打 N 把，
每把从终端重新选本）与 `daily.py`（只认标题里的档位/篇，不看 AP、不看行号）。

耦合评估：mash 现有自动化从 `SupportSelect` 起接管，**完全没有** 终端 → 迦勒底之门 →
每日任务列表 的画面链：`cv.json` 无 ChaldeaGate / DailyQuest 检测，`Screen` 枚举无对应
变体，`Runner` 无终端导航。做 M5 等于新建一条与活动地图平行的导航链（新检测配置 + 新
画面变体 + 档位标题 OCR + 新状态机），复用的只有战斗引擎与苹果恢复。与 EventFarmRuntime
本身没有可共享的状态机（活动刷本的 `EventScanner`/扫图缓存/任务卡规划对每日任务全部
无用），属于里程碑级新特性而非本分支的延伸。

结论：不在活动分支实现。设计要点留档（供后续独立分支使用）：
1. `--phase daily`（harness）与正式页入口并列；配置：档位（初级~超级/地狱级）、篇
   （如「搜集种火」）、最多把数、苹果策略复用现有限额。
2. 画面链 `Terminal → ChaldeaGate → DailyQuestList → PartyConfirm/SupportSelect`；
   选本只认标题 OCR 的档位/篇（活动半减会改 AP、活动期行数不固定，都不看）。
3. 每把打完结算回终端重新选本（沿用 `BattleResultContinue` 非活动分支的重置逻辑）；
   「连续关卡」弹窗关闭而非确认，选本策略保持单点真相。
4. 风险：每日任务列表的标题 OCR 需要新语料；档位按钮样式随活动变化，需实测裁剪。

### P12 打包验证

`pnpm tauri build` 通过：lint / tsc / vitest / vite build / Rust release 全绿，NSIS
安装包产物正常生成。收尾时 updater 签名报 "A public key has been found, but no private
key. Set TAURI_SIGNING_PRIVATE_KEY"——这是发布环境的签名私钥缺失（只影响更新签名步骤），
不是本分支代码缺陷；P12 的验证目标（可构建、可产出安装包）达成。

### 第二夜总结

- **P8 长跑达标**：soak 8e 10/10 连续完成（历史最佳 6/10），`--max-quests` 全流程
  结算→回图→重扫→选本→进本→战斗→结算闭环无人工干预跑通。
- 本夜共修 5 处实机缺陷：①卡片「关闭」连点退出活动（3 秒间隔守卫）、②harness
  `DiagnosticStore` panic、③游戏公告弹窗无识别（`GameNotice`）、④扫描拖动误开
  「上次执行」任务卡（扫描失败恢复重试）、⑤指令码掉落详情页（`CommandCodeDetail`，
  战斗范围通用）。每处均带语料/单测与 docs 更新。
- 苹果：本夜银 11（**超限 1，见 8e 节**）、铜 0、金/彩/圣晶石 0；设备银库存 0/9。
- 遗留：跨运行苹果预算写进 harness；`冠位戴冠战` 11月5日 开启后活动地图入口可能变化，
  届时验证 `GameNotice`/横幅点击链；M5 每日任务按 P11 结论另起分支。


