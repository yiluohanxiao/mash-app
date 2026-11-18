import { useCallback, useEffect, useMemo, useState } from "react";
import { Box, Button, Flex, Select, Text, TextField } from "@radix-ui/themes";
import { ChevronLeftIcon } from "@radix-ui/react-icons";
import { SectionHeading } from "../../components/common/SectionHeading";
import { invoke, listen } from "../../tauri";
import type { BattleApRecoveryItem, Project } from "../../types/project";
import type { Server } from "../../types/server";
import { isAutomationTerminal, type AutomationStatus } from "../../types/automation";
import { buildProjectRunConfig } from "../battle/projectRunConfig";
import type { EventFarmConfig } from "../../types/eventFarm";

interface AutomationEvent {
  status: AutomationStatus;
  currentScreen: string;
  message: string;
}

interface EventFarmPageProps {
  projects: Project[];
  activeProjectId: string | null;
  onProjectSelect: (id: string) => void;
  onBack: () => void;
  onAutomationStart?: () => void;
  onAutomationStartFailed?: () => void;
  onLogEntry?: (message: string) => void;
}

const AP_RECOVERY_LABELS: Record<BattleApRecoveryItem, string> = {
  rainbow: "圣晶石",
  gold: "黄金果实",
  silver: "白银果实",
  bronze: "青铜果实",
  copper: "赤铜果实",
};
const AP_RECOVERY_ORDER: BattleApRecoveryItem[] = [
  "gold",
  "silver",
  "bronze",
  "copper",
  "rainbow",
];

function describeAppleStrategy(project: Project | null): string {
  const configured = project?.apRecoveryItems ?? [];
  const limits = project?.apRecoveryLimits;
  const items = AP_RECOVERY_ORDER.filter(
    (item) => configured.includes(item) && item !== "rainbow",
  );
  if (items.length === 0) {
    return "不使用恢复道具，行动力不足时停止。";
  }
  return (
    items
      .map((item) => {
        const value = limits?.[item];
        return typeof value === "number" && value > 0
          ? `${AP_RECOVERY_LABELS[item]}×${value}`
          : `${AP_RECOVERY_LABELS[item]}不限`;
      })
      .join("、") + "；某种道具达到上限后继续尝试其他道具，全部达到上限后停止。"
  );
}

export function EventFarmPage({
  projects,
  activeProjectId,
  onProjectSelect,
  onBack,
  onAutomationStart,
  onAutomationStartFailed,
  onLogEntry,
}: EventFarmPageProps) {
  const [server, setServer] = useState<Server | null>(null);
  const [targetQuest, setTargetQuest] = useState("");
  const [maxQuestsDraft, setMaxQuestsDraft] = useState("");
  const [running, setRunning] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [currentScreen, setCurrentScreen] = useState("");

  const selectedProject = useMemo(
    () => projects.find((project) => project.id === activeProjectId) ?? projects[0] ?? null,
    [activeProjectId, projects],
  );

  useEffect(() => {
    invoke<Server>("get_server").then(setServer).catch((value) => setError(String(value)));
  }, []);

  useEffect(() => {
    const unlisten = listen<AutomationEvent>("automation-status", (event) => {
      setCurrentScreen(event.payload.currentScreen);
      if (isAutomationTerminal(event.payload)) {
        setRunning(false);
      }
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, []);

  const maxQuests = useMemo(() => {
    const trimmed = maxQuestsDraft.trim();
    if (trimmed === "") return null;
    const value = Number.parseInt(trimmed, 10);
    if (!Number.isInteger(value) || value <= 0) return null;
    return value;
  }, [maxQuestsDraft]);

  const handleStart = useCallback(() => {
    if (!selectedProject || server !== "CN") return;

    setError(null);
    setRunning(true);
    setCurrentScreen("");
    onAutomationStart?.();
    const base = buildProjectRunConfig(selectedProject, {
      repeatMission: false,
      maxMissionRuns: maxQuests,
    });
    // 活动刷本不消耗圣晶石：无论队伍配置如何都不发送 rainbow。
    const eventFarm: EventFarmConfig = {
      targetQuest: targetQuest.trim() === "" ? null : targetQuest.trim(),
    };
    invoke("start_event_farm_automation", {
      config: {
        ...base,
        apRecoveryItems: base.apRecoveryItems.filter((item) => item !== "rainbow"),
        eventFarm,
      },
    }).catch((value) => {
      const message = `启动失败: ${String(value)}`;
      setError(message);
      setRunning(false);
      onLogEntry?.(message);
      onAutomationStartFailed?.();
    });
  }, [
    maxQuests,
    onAutomationStart,
    onAutomationStartFailed,
    onLogEntry,
    selectedProject,
    server,
    targetQuest,
  ]);

  const canStart = !running && server === "CN" && selectedProject != null;

  return (
    <Flex direction="column" className="battle-page event-farm-page">
      <Flex align="center" gap="3" className="battle-header">
        <Button variant="soft" color="gray" onClick={onBack} disabled={running}>
          <ChevronLeftIcon width={16} height={16} />
          <Text size="2">返回</Text>
        </Button>
        <Text size="4" weight="bold">活动刷本</Text>
      </Flex>

      <Flex direction="column" gap="4" className="battle-body battle-body-scroll">
        {server === "JP" && (
          <Box className="event-farm-notice">
            <Text size="2" color="red">活动自动刷本首版仅支持国服。</Text>
          </Box>
        )}

        <Box className="battle-panel">
          <SectionHeading>战斗队伍</SectionHeading>
          <Select.Root
            value={selectedProject?.id ?? ""}
            onValueChange={onProjectSelect}
            disabled={running || projects.length === 0}
          >
            <Select.Trigger placeholder="选择队伍" className="event-farm-team-select" />
            <Select.Content>
              {projects.map((project) => (
                <Select.Item key={project.id} value={project.id}>{project.name}</Select.Item>
              ))}
            </Select.Content>
          </Select.Root>
          <Text size="1" color="gray" className="event-farm-help">
            沿用该队伍的助战条件、战斗指令和行动力恢复设置；忽略普通重复次数。
          </Text>
        </Box>

        <Box className="battle-panel">
          <SectionHeading>刷本目标</SectionHeading>
          <Flex direction="column" gap="3">
            <label>
              <Text size="2" weight="medium">目标关卡名</Text>
              <TextField.Root
                value={targetQuest}
                onChange={(event) => setTargetQuest(event.target.value)}
                placeholder="留空自动选择可打的最高等级本"
                disabled={running}
                maxLength={40}
              />
            </label>
            <label>
              <Text size="2" weight="medium">最多刷本次数</Text>
              <TextField.Root
                value={maxQuestsDraft}
                onChange={(event) => setMaxQuestsDraft(event.target.value)}
                placeholder="不限"
                disabled={running}
                inputMode="numeric"
              />
            </label>
            <Text size="1" color="gray" className="event-farm-help">
              需先在游戏中打开活动地图；扫图结果会缓存 3 小时，失效时现场重新扫描。
              败北后直接停止，不会自动复活。
            </Text>
          </Flex>
        </Box>

        <Box className="battle-panel">
          <SectionHeading>行动力恢复</SectionHeading>
          <Text size="2" color="gray">{describeAppleStrategy(selectedProject)}</Text>
          <Text size="1" color="gray" className="event-farm-help">
            活动刷本不消耗圣晶石；调整恢复道具请到队伍的运行设置。
          </Text>
        </Box>

        {error && <Text size="2" color="red">{error}</Text>}
        {running && (
          <Text size="2" color="gray">当前页面：{currentScreen || "正在启动"}</Text>
        )}
      </Flex>

      <Flex justify="end" align="center" className="page-footer" gap="3">
        {running ? (
          <Button color="red" variant="soft" onClick={() => void invoke("stop_automation")}>停止</Button>
        ) : (
          <Button disabled={!canStart} onClick={handleStart}>开始活动刷本</Button>
        )}
      </Flex>
    </Flex>
  );
}
