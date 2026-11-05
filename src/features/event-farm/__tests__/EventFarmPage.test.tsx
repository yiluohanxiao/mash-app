import { describe, expect, it, vi } from "vitest";
import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { invoke } from "@tauri-apps/api/core";
import { renderWithTheme } from "../../../test/renderWithTheme";
import type { Project } from "../../../types/project";
import { EventFarmPage } from "../EventFarmPage";

const PROJECT: Project = {
  id: "team-1",
  name: "活动队伍",
  supportServantId: null,
  supportServantVariantKey: null,
  repeatMission: true,
  repeatMode: "count",
  repeatCount: 99,
  // 圣晶石即使被队伍配置勾选，活动刷本也不应发送。
  apRecoveryItems: ["rainbow", "silver", "copper"],
  apRecoveryLimits: { silver: 10, copper: 10 },
  slots: [
    { id: "owned-1", type: "servant", servantId: 1 },
    { id: "support-1", type: "support", servantId: null },
  ],
};

function mockCommands(server: "CN" | "JP") {
  vi.mocked(invoke).mockImplementation(async (command: string) => {
    if (command === "get_server") return server;
    return null;
  });
}

function renderEventFarmPage() {
  return renderWithTheme(
    <EventFarmPage
      projects={[PROJECT]}
      activeProjectId="team-1"
      onProjectSelect={() => {}}
      onBack={() => {}}
    />,
  );
}

describe("EventFarmPage", () => {
  it("starts event farm automation with the target quest and run cap", async () => {
    const user = userEvent.setup();
    mockCommands("CN");
    renderEventFarmPage();

    await user.type(
      await screen.findByRole("textbox", { name: "目标关卡名" }),
      "M星之旅",
    );
    await user.type(
      screen.getByRole("textbox", { name: "最多刷本次数" }),
      "3",
    );
    await user.click(screen.getByRole("button", { name: "开始活动刷本" }));

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith("start_event_farm_automation", {
        config: expect.objectContaining({
          projectId: "team-1",
          maxMissionRuns: 3,
          eventFarm: { targetQuest: "M星之旅" },
        }),
      });
    });
  });

  it("never sends saint quartz even when the team config enables it", async () => {
    const user = userEvent.setup();
    mockCommands("CN");
    renderEventFarmPage();

    await user.click(await screen.findByRole("button", { name: "开始活动刷本" }));

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith(
        "start_event_farm_automation",
        expect.anything(),
      );
    });
    const call = vi
      .mocked(invoke)
      .mock.calls.find(([cmd]) => cmd === "start_event_farm_automation");
    const { config } = (call as unknown as [
      string,
      { config: { apRecoveryItems: string[]; eventFarm: { targetQuest: string | null } } },
    ])[1];
    expect(config.apRecoveryItems).not.toContain("rainbow");
    expect(config.apRecoveryItems).toEqual(["silver", "copper"]);
    expect(config.eventFarm).toEqual({ targetQuest: null });
  });

  it("describes the apple strategy from the team config", async () => {
    mockCommands("CN");
    renderEventFarmPage();

    expect(
      await screen.findByText(/白银果实×10、赤铜果实×10/)
    ).toBeInTheDocument();
    expect(screen.getByText(/不消耗圣晶石/)).toBeInTheDocument();
  });

  it("stops a running farm loop", async () => {
    const user = userEvent.setup();
    mockCommands("CN");
    renderEventFarmPage();

    await user.click(await screen.findByRole("button", { name: "开始活动刷本" }));
    const stop = await screen.findByRole("button", { name: "停止" });
    await user.click(stop);

    expect(invoke).toHaveBeenCalledWith("stop_automation");
  });

  it("disables start on JP and shows the region notice", async () => {
    mockCommands("JP");
    renderEventFarmPage();

    expect(await screen.findByText(/仅支持国服/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "开始活动刷本" })).toBeDisabled();
  });
});
