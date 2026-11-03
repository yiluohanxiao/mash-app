# 活动自动化

目标是把活动流程串成一条闭环：终端 → 活动地图 → 扫描地图 → 选最高等级关卡 → 进关 →
交给现有战斗引擎 → 结算 → 回地图。点数活动的主线按点数解锁，因此策略是：主线未解锁时
打当前最高等级关卡攒点数，主线解锁后打主线，主线全部结束后回到最高等级关卡。

生命周期、模板错误和失败诊断见[公共约定](README.md)。参考实现来自 `reference/fgo-auto/`
（只读导出，已被 git 忽略），逐函数映射见 [event-farm-plan.md](../event-farm-plan.md)。

## 当前范围（M4）

已完成：

- `Screen` 共 20 个活动画面：M0 的 `EventMap`、`Terminal`、`NodeDetail`、`QuestConfirm`、
  `Story`、`SkipConfirm`、`TapContinue`、`Loading`、`Title`、`Defeat`、`Busy`、`Cg`，
  加 M1 的 `RewardSelect`、`RewardPage`、`ServantFull`、`Gate`、`Tip`、`PartyConfirm`、
  `RepeatConfirm`、`ListUpdate`。
- `cn/cv.json` 对应关键词/亮度探针；`ListUpdate` 用 `overrides` 抢回被 `SupportSelect`
  模板命中的刷新确认框。
- sidecar 识别为三段式：模板 → 关键词 → 亮度，先命中先返回；模板命中可被 `overrides`
  里的关键词探针覆盖。
- sidecar 新增 `read_map_labels` / `read_quest_card` / `read_event_points` /
  `frame_thumbnail` 四个 REPL 动词，逻辑在 `mash_cv/event_map.py`。
- 触控层新增双指 `pinch`：`TouchBackend` 接口、scrcpy 多点包、ADB `sendevent` 兜底，
  仍经人类化抖动。
- `runner/event/`：地图平移/边界/归位、标签 OCR、关卡卡解析、扫图缓存（3 小时）、
  增量扫描与合并。
- 调试命令 `debug_scan_event_map` 与前端「扫描活动地图」按钮，实机验收靠它。
- M2 进关：`EventFarmRuntime` 驱动 `EventMap → NodeDetail → QuestConfirm`，`Gate` 退出，
  从 `SupportSelect` 起交给战斗引擎；入口是战斗页「活动刷本」开关。
- M3 循环与恢复：结算页点「结束」回活动地图，回图后增量重扫再选下一本；
  `Story`/`SkipConfirm`/`TapContinue`/`Tip`/`ListUpdate`/`RepeatConfirm`/`ServantFull`/
  `Defeat`/`PartyConfirm` 九个活动画面接入事件处理器（坐标取参考实现 `_common`）；
  连续进关失败退避 5/10/20/40/60 秒，同一标签连败 2 次本轮内拉黑（仅内存，不落盘）。
  `max_mission_runs` 与「打完本轮停止」照常生效，达到上限仍停止。
- M4 点数策略与苹果：未指定目标关卡时由 `plan_once` 决策（推进主线 ↔ 刷点数）；
  AP 不足时活动模式固定 银→青铜→赤铜，各限 `EVENT_APPLE_LIMIT = 10`，用户配置只能
  下调；金/彩/圣晶石在活动模式硬编码为不可用。

未完成（后续里程碑）：每日任务（M5）、正式活动页面（现用战斗页开关）。
`Loading`、`Title`、`Busy`、`Cg`、`RewardSelect`、`RewardPage` 在活动模式下仍只
等待不点击。

## 识别顺序

`_detect_screen` 依次执行三段探针，某段有结果即返回：

1. **模板**：现有战斗/结算画面，行为与旧版一致，命中即短路。
2. **关键词**：`keywords`（任一命中）或 `keywordGroups`（各组都命中），按 `priority`
   高者优先，同优先级比分数。OCR 按区域缓存，多个画面共用同一探针框只识别一次。
3. **亮度**：`blank` 的帧均值区间，用于没有文字的纯黑加载页。

模板必须先行，否则活动画面的高优先级关键词会在战斗帧上盖过 `Battle`；这与参考实现
“便宜且最能区分的探针先走”的顺序一致。

关键词按 **单个 OCR 片段** 做子串匹配，不跨片段拼接。中文标题常被 OCR 切成两段
（`冠位编队` + `确认`），所以关键词要选能在单段内出现的短形式，而不是整句。

### 模板覆盖（overrides）

确认框画在它所在的页面上，底下的模板仍然命中：`将更新「助战选择」界面的成员列表`
弹窗会被 `shared/screen_support_select` 模板认成 `SupportSelect`。这类画面在自己的
`detect` 里写 `"overrides": ["SupportSelect"]`；模板段命中这些屏幕时，只对声明了该覆盖
的画面再跑一次关键词段，命中则改用关键词结果，否则保留模板结果。没有声明 `overrides`
的帧不会多花一次 OCR。

## 画面路由

| 画面 | 探针 | 优先级 | 说明 |
| --- | --- | --- | --- |
| `Defeat` | 底部按钮行 `使用圣晶石继续` / `圣晶石继续` / `撤退` | 80 | 最高优先级；失败页绝不能点圣晶石复活 |
| `ServantFull` | 中带 `从者数已超上限` / `从者空位` | 78 | 从者已满弹窗；出现即停，不能替用户删从者 |
| `RewardSelect` | 顶带 `活动奖励画面选择` | 75 | 奖励选择页；只读 |
| `Title` | 左下 `版本号` | 70 | 游戏被重启后回到标题页 |
| `Gate` | 右上 `迦勒底之门`/`迦勒底之`/`勒底之` 且左上 `关闭` | 65 | 自由本列表；与 `NodeDetail` 共用 `关闭`，必须压过它 |
| `EventMap` | 左上 `管理室` | 60 | 活动地图；区域标签位置需 OCR 现找，不能写死 |
| `Terminal` | 左上 `通知` | 55 | 终端；点活动横幅进入地图 |
| `QuestConfirm` | 中带 `是否开始` / `开始关卡` / `是否开始任务` / `该任务没有战斗` / `任务开始` / `败北` / `无法在` / `该关卡中` | 50 | 开始任务确认框（含特殊规则提示） |
| `Tip` | 中带 `该支援角色不存在` / `小贴士` | 48 | 提示弹窗；等待或关闭，绝不点确定以外的位置 |
| `SkipConfirm` | 右上 `跳过` 且中带 `是否跳过` / `跳过该段` | 46 | 跳过剧情确认 |
| `Story` | 右上 `跳过` | 45 | 剧情页 |
| `PartyConfirm` | 顶右 `冠位编队` / `助战编队` | 44 | 编队确认框；比 `Title`/`Tip` 低，避免抢走同区文字 |
| `ListUpdate` | 中带 `将更新` / `成员列表`，`overrides: ["SupportSelect"]` | 43 | 助战列表刷新确认框；模板先命中 `SupportSelect` |
| `RepeatConfirm` | 中带 `连续进行同一关卡` / `连续出击` | 42 | 连续出击确认；只有明确要求时才点“是” |
| `RewardPage` | 右上 `点数报酬` / `道具兑换` | 40 | 点数报酬页；只读 |
| `TapContinue` | 中下 `请点击游戏界面` | 35 | 通用“点击继续” |
| `NodeDetail` | 左上 `关闭` 且卡片区 `消耗` / `任务进行度` / `首次报酬` / `推荐` | 30 | 任务信息卡；两段都命中才成立 |
| `Loading` | 右下 `加载中`，或整帧均值 ≤ 45 | 20 | 黑屏加载页没有文字，走亮度兜底 |
| `Busy` | 中下 `该操作无效` | 15 | 瞬时 toast；等下一帧再操作 |
| `Cg` | 无探针 | — | 全屏 CG/演出没有可读文字，观测为 `Unknown` |

优先级是安全规则，不是美学：`Defeat` 必须压过所有结算画面，`EventMap`/`Terminal` 必须
压过通用卡片探针。`tests/test_event_corpus.py` 固定了这张表的相对顺序。

`NodeDetail` 采用“关闭 + 卡片关键词”的合取，而不是只看左上 `关闭`：`迦勒底之门`、
`每日任务` 等页面同样有 `关闭`，只看它会误判，从而在自由本列表上盲点卡片 ▶ 的位置。
M1 已把 `迦勒底之门` 单列为 `Gate`（优先级 65 > 30），`每日任务` 仍落到 `NodeDetail`。

## 地图扫描

扫图目标是一屏放下整张地图，再逐个打开区域标签读卡片。步骤在
`runner/event/scan.rs`：

1. 等待 `EventMap`；地图可能刚从结算页返回，缩放状态已丢失。
2. 双指捏合缩到最小（最多 4 轮，每轮 600→120 像素、300 ms），每轮后重新识别地图。
3. 归位：先向右拖（`pan` +X），再向上拖（`pan` +Y），直到缩略图判定到边界。
4. OCR 读全部区域标签（`read_map_labels`），过滤非标签文本与贴边被裁掉的标签，按文本去重。
5. 逐个点标签 → 等 `NodeDetail` → `read_quest_card` → 关闭，只保留真实关卡卡。
6. `read_event_points` 读点数与主线进度；`best_farm_quest` 选最高等级可打关卡。

边界判定不看画面坐标，而是比较拖动前后 64×36 缩略图的平均像素差（`frame_thumbnail`）：
差值 < 3.0 说明画面没动，已经到边。地图边缘是空地，没有可锚定的特征，这比模板可靠。

标签少于 3 个时直接报错「二维拖图扫描将在导航运行时接入」，而不是在半张地图上瞎点：
M1 只做「缩到最小后一屏放下」这一种扫法。

扫图结果写 `app_data_dir()/event/scout.json`，3 小时内有效。再次扫描时只重读新出现的标签
和上次锁定的标签（`delta_targets`），合并时同名卡片以新结果为准；同一标签既读到锁定卡
又读到解锁卡时保留解锁卡（`merge_delta`）。

## 进关流程（M3）

活动模式由现有 `Runner` 内的 `EventFarmRuntime` 驱动（决策 A：不建第二个 runner 进程，
沿用 rank-up quest 的挂载方式）。启动命令 `start_event_farm_automation` 复用战斗启动链
（设备租约记为 `AutomationKind::EventFarm`），识别范围切到 `use_event_detection()`；
主循环把 13 个活动画面路由进 `runner/event/farm.rs`：

| 画面 | 动作 | 之后 |
| --- | --- | --- |
| `EventMap` | 取扫图缓存（3 小时内）或现场扫描（结算回来先增量重扫）；`decide_target` 选卡（指定名按名匹配，否则 `plan_once`，黑名单标签排除）；OCR 现找标签坐标并点击 | → `NodeDetail` |
| `NodeDetail` | 等待进卡：先 `read_quest_card` 实读校验与缓存选卡一致（不一致则实读为准重决策，换卡即关闭卡片回图重选），再点 ▶（参考坐标 1370,320） | → `QuestConfirm` / 助战链 |
| `QuestConfirm` | 点「开始任务」（参考坐标 1259,846）；成功即清零失败连击 | 助战/队伍/战斗由现有处理器接管 |
| `Gate` | 进关途中误入自由本列表：只点左上「关闭」 | 回 `EventMap` 重试 |
| `Story` / `SkipConfirm` | 右上「跳过」（1786,60）→ 确认「是」（1241,840） | 主线解锁剧情被跳过 |
| `TapContinue` | 点「请点击游戏界面」避开 toast 的位置（960,400） | 继续流程 |
| `Tip` | 只点「退出」（975,845），绝不点确定/充值 | 弹窗消失 |
| `ListUpdate` | 点「否」（592,755），不刷新助战列表 | 弹窗消失 |
| `RepeatConfirm` | 点「关闭」（700,875），不点「连续出击」 | 由选卡策略决定下一个本 |
| `ServantFull` | 点「关闭」（860,720），不变还/不删除从者 | 停止，等人工清理 |
| `Defeat` | 不点击，绝不圣晶石复活 | 停止，等人工 |
| `PartyConfirm` | 不点击，避免意外编队；连续观测超 40 帧仍未消失则停止 | 停止，等人工确认编队 |

同一标签最多点 3 次：地图上重新观测到 `EventMap`（卡片被点掉）或 OCR 连续找不到标签
都会计入。超限把该标签计入黑名单、按连续失败次数退避（5/10/20/40/60 秒封顶）后换下一个
关卡，而不是停止整个任务；地图上找不到标签时先等待重试，不猜坐标。黑名单仅存于内存
（`bad_labels`，上限 2 次），自动化重启即清空。

非等待状态下的任务卡（结算回来卡片还开着等遗留场景）点左上「关闭」退出，但点击之间
强制间隔 3 秒：卡片关闭后该坐标就是活动地图左上角的返回箭头，连点会把流程退出活动
（soak 8 实测踩过，表现为主页 Unknown 循环直到停止）。间隔点击超过 5 次卡片仍未消失
则停止等人工。回图后计数清零。

结算页 `BattleResultContinue` 在活动模式下先做与重复任务相同的每轮重置（轮数 +1、
队伍/战斗状态清零），未达 `max_mission_runs` 且未勾选「打完本轮停止」时点「结束」→
`AwaitingReturnToMap`，下一次 `EventMap` 观测触发增量重扫并继续循环；达到上限或勾选
停止则照旧 `Finished`。

从 `SupportSelect` / `TeamConfirm` / `Battle` / 结算 onward 全部走现有战斗处理器，
队伍、助战、技能配置沿用所选编队项目。

入口：战斗页脚的「活动刷本」开关。打开后「开始任务」改发
`start_event_farm_automation`，配置与普通战斗完全一致；关闭后行为不变。
启动要求游戏已停在活动地图，且区服为 CN。

## 触控：双指缩放

`TouchBackend::pinch(center, from_gap, to_gap, duration_ms)` 的 gap 是**两指总间距**，
不是半间距：参考实现的 300→60 在这里写作 600→120。

- scrcpy 后端（默认）：两个 pointer（0 左、1 右）同时 DOWN，交错 MOVE，最后各自 UP；
  单帧最多 604 个事件，超出即拒绝。侧车 `validate_events` 按 pointer 追踪按下状态，
  结束时仍有指针未抬起会直接报错。
- ADB 后端（兜底）：`adb shell` 脚本走 MT-B 槽位事件，坐标按 rotation-1 换算为
  `(screen_h - y, x)`；设备节点默认 `/dev/input/event4`，可用 `MASH_TOUCH_DEVICE` 覆盖，
  且只允许字母数字与 `/ _ - .`。

## 失败页与不可点区域

- `Defeat` 只允许“撤退”，永远不点圣晶石复活。识别不到底部按钮时保持等待。
- `Busy` 表示上一次操作无效：不重试，等下一帧重新识别。
- `Cg` 与纯黑/纯白无文字页面都按等待处理；参考实现用 150 秒（CG）与 480 秒（加载）
  的卡死上限区分，mash 当前不主动重启游戏。

## 已知缺口

- 白底 `连接中` 页（语料 `59_connecting.png`）没有 `加载中` 文字，暂观测为 `Unknown`；
  行为与 `Loading` 相同（等待），待补充亮度上限规则后再单独命名。
- JP 未配置活动画面；活动模式当前只支持 CN。
- `每日任务` 页仍观测为 `NodeDetail`；M5 做日常时再单列。
- 二维拖图兜底扫描（地图缩到最小仍放不下时按网格拖动）未实现；少于 3 个标签直接报错。
- ADB 捏合假设 1080×1920、rotation-1 的面板和 `event4` 节点，换设备需要实测校准。
- M1 验收（8 个标签、卡片数值与手工核对一致）仍需在实机上用调试页「扫描活动地图」确认；
  离线只验证到解析逻辑与语料画面识别。

## 点数活动策略（M4）

未指定目标关卡时，选卡由 `plan_once`（`runner/event/card.rs`，移植参考
`flow.plan_once`）按当前卡池与点数决策，黑名单标签先行剔除：

1. 有锁定的主线卡且最低门槛点数已达标 → 进该主线（游戏在门槛处自动解锁）。
2. 有锁定的主线卡但点数不足 → 先刷最高等级可打本攒点。
3. 有解锁的主线卡 → 推等级最低的一段主线。
4. 主线推完或没有主线 → 刷最高等级可打本。

点数在选卡前现场读取（`read_event_points`）；读不到按 0 处理，只会更保守地刷点，
不会跳过攒点去硬闯门槛。指定了目标关卡时跳过该策略，仍按名匹配最高等级。

地图缩放状态在离开地图后会丢失，每次回到地图都要重新校正；结算回图后的增量重扫
（见「进关流程」）保证新解锁的主线卡会被读到。

## 苹果策略（M4）

AP 不足时 `Screen::APRecovery` 走现有回复流程，活动模式覆盖道具策略
（`runner/ap_recovery.rs`）：

- 固定顺序 银 → 青铜 → 赤铜（`EVENT_AP_RECOVERY_ITEMS`），不复用项目配置的顺序。
- 每个类型上限 `EVENT_APPLE_LIMIT = 10`（代码常量）：用户配置只能下调，配置更高也会
  被钳到 10。计数沿用 `BattleRunApRecoveryUsage`，吃一次记一次。
- 金苹果、彩虹苹果（圣晶石回复）在活动模式硬编码上限 0，任何配置都不可用；
  圣晶石复活路径（`Defeat`）也从不点击。
- 所有可用道具用尽 → 「已配置苹果数量不足，停止」，正常结束而不是报错。

## 测试

- `sidecar/cv-code/tests/test_event_detect.py`：关键词、合取组、`overrides`、OCR 缓存、
  亮度探针的合成用例。
- `sidecar/cv-code/tests/test_event_map.py`：标签过滤/贴边裁剪、关卡卡名称/等级/`90++`/AP/
  锁定/点数门槛、点数读取的兜底路径。
- `sidecar/cv-code/tests/test_event_corpus.py`：用 `reference/fgo-auto/tests/corpus` 的真实截图
  回归；导出目录不存在时自动跳过。语料约 8 分钟，覆盖正样本与战斗/结算负样本。
- `sidecar/cv-code/tests/test_scrcpy_control.py`：双指包格式、指针校验、中断时释放所有指针。
- `cargo test --manifest-path src-tauri/Cargo.toml`：`Screen` 往返、检测范围、`cn/cv.json`
  活动探针的优先级顺序与 `overrides` 目标合法性；`runner/event/tests.rs` 覆盖卡片解析、
  平移几何、帧差边界、标签去重、缓存 TTL、增量合并；`runner/event/farm.rs` 覆盖选卡
  （指定名 / 回退最高等级 / 锁定排除 / 黑名单标签排除）、退避序列、对话框参考坐标、
  阶段初值；`runner/event/card.rs`（经 `runner/event/tests.rs`）覆盖 `plan_once`
  四个决策分支、黑名单剔除与「无可推进关卡」报错、`is_story`/`is_farm`/门槛解析；
  `runner/tests.rs` 覆盖活动模式苹果钳制与道具用尽停止；`touch/scrcpy.rs` 覆盖捏合事件序列。
- `src/features/battle/__tests__/BattlePage.test.tsx`：「活动刷本」开关把启动命令切到
  `start_event_farm_automation` 并携带 `eventFarm`。
- `src/features/debug/__tests__/DebugPage.test.tsx`：扫描按钮把标签/点数/卡片写入日志。
