import { useCallback, useEffect, useState } from "react";
import { commands } from "../ipc";
import type {
  MissionInfo,
  MissionAgentInfo,
  DiffFile,
  ReviewAction,
} from "../ipc";
import { AgentReviewTabs } from "../components/review/AgentReviewTabs";
import { DiffFileTree } from "../components/review/DiffFileTree";
import { DiffViewer } from "../components/review/DiffViewer";
import { ReviewActionBar } from "../components/review/ReviewActionBar";
import styles from "./ReviewView.module.css";

function usePrefersDark(): boolean {
  const [dark, setDark] = useState(
    () => window.matchMedia("(prefers-color-scheme: dark)").matches,
  );
  useEffect(() => {
    const mql = window.matchMedia("(prefers-color-scheme: dark)");
    const handler = (e: MediaQueryListEvent) => setDark(e.matches);
    mql.addEventListener("change", handler);
    return () => mql.removeEventListener("change", handler);
  }, []);
  return dark;
}

export function ReviewView() {
  const prefersDark = usePrefersDark();

  const [missions, setMissions] = useState<MissionInfo[]>([]);
  const [selectedMissionId, setSelectedMissionId] = useState<string | null>(null);
  const [agents, setAgents] = useState<MissionAgentInfo[]>([]);
  const [selectedAgentId, setSelectedAgentId] = useState<string | null>(null);
  const [diffFiles, setDiffFiles] = useState<DiffFile[]>([]);
  const [selectedFilePath, setSelectedFilePath] = useState<string | null>(null);
  const [reviewStatuses, setReviewStatuses] = useState<Record<string, ReviewAction | null>>({});
  const [loadingDiff, setLoadingDiff] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Detect forced theme from document attribute
  const docTheme = document.documentElement.getAttribute("data-theme");
  const editorTheme: "light" | "dark" =
    docTheme === "dark" ? "dark" : docTheme === "light" ? "light" : prefersDark ? "dark" : "light";

  useEffect(() => {
    commands.listMissions().then((list) => {
      const reviewable = list.filter((m) => m.status === "running" || m.status === "completed");
      setMissions(reviewable);
      if (reviewable.length > 0 && !selectedMissionId) {
        setSelectedMissionId(reviewable[0].id);
      }
    });
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    if (!selectedMissionId) {
      setAgents([]);
      setSelectedAgentId(null);
      return;
    }

    commands.listAgentsByMission(selectedMissionId).then((list) => {
      setAgents(list);
      if (list.length > 0) {
        setSelectedAgentId(list[0].id);
      } else {
        setSelectedAgentId(null);
      }
      setDiffFiles([]);
      setSelectedFilePath(null);
    });
  }, [selectedMissionId]);

  const loadDiff = useCallback(async (agentId: string) => {
    setLoadingDiff(true);
    setError(null);
    try {
      const resp = await commands.getAgentDiff(agentId);
      setDiffFiles(resp.files);
      setReviewStatuses((prev) => ({ ...prev, [agentId]: resp.review_status }));
      if (resp.files.length > 0) {
        setSelectedFilePath(resp.files[0].path);
      } else {
        setSelectedFilePath(null);
      }
    } catch (e) {
      setError(String(e));
      setDiffFiles([]);
      setSelectedFilePath(null);
    } finally {
      setLoadingDiff(false);
    }
  }, []);

  useEffect(() => {
    if (selectedAgentId) {
      loadDiff(selectedAgentId);
    }
  }, [selectedAgentId, loadDiff]);

  const handleReviewAction = useCallback(
    async (action: ReviewAction, comment?: string) => {
      if (!selectedAgentId) return;
      try {
        await commands.submitReviewAction({
          agent_id: selectedAgentId,
          action,
          comment,
        });
        setReviewStatuses((prev) => ({ ...prev, [selectedAgentId]: action }));
      } catch (e) {
        setError(String(e));
      }
    },
    [selectedAgentId],
  );

  const selectedFile = diffFiles.find((f) => f.path === selectedFilePath) ?? null;

  if (missions.length === 0) {
    return (
      <div className={styles.container}>
        <div className={styles.header}>
          <span className={styles.headerTitle}>Code Review</span>
        </div>
        <div className={styles.emptyState}>
          <svg className={styles.emptyIcon} viewBox="0 0 48 48" fill="none">
            <path
              d="M12 24L20 32L36 16"
              stroke="currentColor"
              strokeWidth="3"
              strokeLinecap="round"
              strokeLinejoin="round"
            />
          </svg>
          <span className={styles.emptyText}>
            No running or completed missions to review
          </span>
        </div>
      </div>
    );
  }

  return (
    <div className={styles.container}>
      <div className={styles.header}>
        <span className={styles.headerTitle}>Code Review</span>
        <select
          className={styles.missionSelect}
          value={selectedMissionId ?? ""}
          onChange={(e) => setSelectedMissionId(e.target.value || null)}
        >
          {missions.map((m) => (
            <option key={m.id} value={m.id}>
              {m.title} ({m.status})
            </option>
          ))}
        </select>
      </div>

      <div className={styles.body}>
        <AgentReviewTabs
          agents={agents}
          selectedAgentId={selectedAgentId}
          reviewStatuses={reviewStatuses}
          onSelect={setSelectedAgentId}
        />

        {error && (
          <div className={styles.loading} style={{ color: "var(--color-error)" }}>
            {error}
          </div>
        )}

        {loadingDiff ? (
          <div className={styles.loading}>Loading diff...</div>
        ) : agents.length === 0 ? (
          <div className={styles.emptyState}>
            <span className={styles.emptyText}>No agents for this mission</span>
          </div>
        ) : (
          <div className={styles.diffArea}>
            <DiffFileTree
              files={diffFiles}
              selectedPath={selectedFilePath}
              onSelect={setSelectedFilePath}
            />
            <DiffViewer file={selectedFile} theme={editorTheme} />
          </div>
        )}

        {selectedAgentId && (
          <ReviewActionBar
            currentStatus={reviewStatuses[selectedAgentId] ?? null}
            disabled={loadingDiff || diffFiles.length === 0}
            onAction={handleReviewAction}
          />
        )}
      </div>
    </div>
  );
}
