import "@testing-library/jest-dom/vitest";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import { DeliveryWorkspace } from "./DeliveryWorkspace";
import { commands } from "../../ipc/commands";

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

function deliverySnapshot(overrides: Record<string, unknown> = {}) {
  return {
    mission_id: "mission-1",
    status: "completed",
    generated_at: "2026-06-16T10:00:00Z",
    result: "Completed the requested feature and verified it.",
    overview: "Delivery summary for the finished mission.",
    warnings: [],
    primary_delivery: {
      label: "Feature package",
      path: "/tmp/miragenty/releases/mission-1.zip",
      summary: "Ready-to-use release package.",
    },
    how_to_use: ["Unzip the package", "Run pnpm install", "Run pnpm test"],
    validation: [
      { label: "Focused tests", status: "passed", detail: "pnpm test src/components/mission/DeliveryWorkspace.test.tsx" },
    ],
    supporting_deliverables: [
      { label: "Implementation notes", path: "docs/notes.md", summary: "Notes for maintainers." },
    ],
    handoff_timeline: [
      { title: "Implemented UI", detail: "Added completed-state delivery workspace.", timestamp: "2026-06-16T10:00:00Z" },
    ],
    report_id: "report-1",
    ...overrides,
  };
}

describe("DeliveryWorkspace", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockCommands.listChatMessages.mockResolvedValue([]);
  });

  it("renders a persisted delivery snapshot with the primary deliverable path and follow-up chat", async () => {
    mockCommands.getMissionDelivery.mockResolvedValue(deliverySnapshot());

    render(<DeliveryWorkspace missionId="mission-1" missionStatus="completed" />);

    expect(await screen.findByText("Delivery Workspace")).toBeInTheDocument();
    expect(screen.getByText("/tmp/miragenty/releases/mission-1.zip")).toBeInTheDocument();
    expect(screen.getByText("Follow-up Chat")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /view full report/i })).toBeInTheDocument();
    expect(mockCommands.generateMissionDelivery).not.toHaveBeenCalled();
  });

  it("generates and reloads when no delivery snapshot exists", async () => {
    mockCommands.getMissionDelivery
      .mockResolvedValueOnce(null)
      .mockResolvedValueOnce(deliverySnapshot({ result: "Generated delivery snapshot." }));
    mockCommands.generateMissionDelivery.mockResolvedValue({ report_id: "report-1", generated_at: "2026-06-16T10:00:00Z" });

    render(<DeliveryWorkspace missionId="mission-1" missionStatus="completed" />);

    expect(await screen.findByText("Preparing delivery summary…")).toBeInTheDocument();
    await waitFor(() => expect(mockCommands.generateMissionDelivery).toHaveBeenCalledWith("mission-1"));
    expect(await screen.findByText("Generated delivery snapshot.")).toBeInTheDocument();
    expect(mockCommands.getMissionDelivery).toHaveBeenCalledTimes(2);
  });

  it("warns when the snapshot has no obvious delivery package", async () => {
    mockCommands.getMissionDelivery.mockResolvedValue(
      deliverySnapshot({
        primary_delivery: null,
        supporting_deliverables: [],
      }),
    );

    render(<DeliveryWorkspace missionId="mission-1" missionStatus="failed" />);

    expect(await screen.findByText("No obvious delivery package was found. Review the partial outputs below or ask in follow-up chat."))
      .toBeInTheDocument();
    expect(screen.getByText("Follow-up Chat")).toBeInTheDocument();
  });
});
