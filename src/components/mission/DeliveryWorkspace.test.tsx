import "@testing-library/jest-dom/vitest";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import { DeliveryWorkspace } from "./DeliveryWorkspace";
import { commands, type MissionDeliveryView } from "../../ipc/commands";

vi.mock("../../ipc/commands", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../ipc/commands")>();
  return {
    ...actual,
    commands: {
      ...actual.commands,
      getMissionDelivery: vi.fn(),
      generateMissionDelivery: vi.fn(),
      listChatMessages: vi.fn().mockResolvedValue([]),
    },
  };
});

vi.mock("../../ipc/events", () => ({
  onChatStream: vi.fn().mockResolvedValue(() => {}),
  onFollowupProposed: vi.fn().mockResolvedValue(() => {}),
}));

const mockCommands = vi.mocked(commands);

function deliveryView(overrides: Partial<MissionDeliveryView["snapshot"]> = {}): MissionDeliveryView {
  const snapshot: MissionDeliveryView["snapshot"] = {
    schema_version: 1,
    mission_id: "mission-1",
    status: "completed_with_warnings",
    confidence: "low",
    overview: {
      title: "Degraded mission delivery snapshot",
      summary: "Delivery summary for the finished mission.",
      status: "completed_with_warnings",
      confidence: "low",
    },
    items: [
      {
        id: "package",
        source: "artifact",
        title: "Feature package",
        summary: "Ready-to-use release package.",
        file_paths: ["/tmp/miragenty/releases/mission-1.zip"],
        confidence: "medium",
      },
      {
        id: "notes",
        source: "handoff",
        title: "Implementation notes",
        summary: "Notes for maintainers.",
        file_paths: ["docs/notes.md"],
        confidence: "medium",
      },
    ],
    how_to_use: [{ title: "Review deliverables", detail: "Unzip the package and run pnpm test." }],
    validation: [
      {
        status: "passed",
        summary: "Focused tests passed.",
        command: "pnpm test src/components/mission/DeliveryWorkspace.test.tsx",
      },
    ],
    changes: [
      { title: "Implemented UI", detail: "Added completed-state delivery workspace.", files: [] },
    ],
    caveats: [],
    next_steps: ["Ask for notarization"],
    ...overrides,
  };
  return {
    mission_id: "mission-1",
    version: 1,
    generation_status: "degraded",
    curator_model: "deterministic-fallback",
    source_task_ids: "[]",
    source_event_ids: "[]",
    stale: false,
    created_at: "2026-06-16T10:00:00Z",
    updated_at: "2026-06-16T10:00:00Z",
    snapshot,
  };
}

describe("DeliveryWorkspace", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockCommands.listChatMessages.mockResolvedValue([]);
  });

  it("renders a persisted delivery snapshot with the primary deliverable path and follow-up chat", async () => {
    mockCommands.getMissionDelivery.mockResolvedValue(deliveryView());

    render(<DeliveryWorkspace missionId="mission-1" missionStatus="completed" />);

    expect(await screen.findByText("Delivery Workspace")).toBeInTheDocument();
    expect(screen.getByText("/tmp/miragenty/releases/mission-1.zip")).toBeInTheDocument();
    expect(screen.getByText("Follow-up Chat")).toBeInTheDocument();
    expect(mockCommands.generateMissionDelivery).not.toHaveBeenCalled();
  });

  it("generates and uses the response when no delivery snapshot exists", async () => {
    mockCommands.getMissionDelivery.mockResolvedValueOnce(null);
    mockCommands.generateMissionDelivery.mockResolvedValue({
      mission_id: "mission-1",
      generation_status: "degraded",
      delivery: deliveryView({ overview: { title: "Generated", summary: "Generated delivery snapshot.", status: "completed_with_warnings", confidence: "low" } }),
    });

    render(<DeliveryWorkspace missionId="mission-1" missionStatus="completed" />);

    await waitFor(() => expect(mockCommands.generateMissionDelivery).toHaveBeenCalledWith("mission-1"));
    expect(await screen.findByText("Generated delivery snapshot.")).toBeInTheDocument();
    expect(mockCommands.getMissionDelivery).toHaveBeenCalledTimes(1);
  });

  it("warns when the snapshot has no obvious delivery package", async () => {
    mockCommands.getMissionDelivery.mockResolvedValue(
      deliveryView({
        items: [],
        status: "failed",
        overview: { title: "Failed delivery", summary: "Partial delivery snapshot.", status: "failed", confidence: "low" },
      }),
    );

    render(<DeliveryWorkspace missionId="mission-1" missionStatus="failed" />);

    expect(await screen.findByText("No obvious delivery package was found. Review the partial outputs below or ask in follow-up chat."))
      .toBeInTheDocument();
    expect(screen.getByText("Follow-up Chat")).toBeInTheDocument();
  });
});
