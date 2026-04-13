import { useCallback, useEffect, useRef, useState } from "react";
import { commands, onEvaluationComplete } from "../ipc";
import type {
  MissionInfo,
  MissionAgentInfo,
  DiffFile,
  ReviewAction,
  EvaluationResult,
  AnnotationInfo,
} from "../ipc";
import { AgentReviewTabs } from "../components/review/AgentReviewTabs";
import { DiffFileTree } from "../components/review/DiffFileTree";
import { DiffViewer } from "../components/review/DiffViewer";
import { ReviewActionBar } from "../components/review/ReviewActionBar";
import { ReviewFilterBar, type ReviewFilter } from "../components/review/ReviewFilterBar";
import { EvalSummaryBar } from "../components/review/EvalSummaryBar";
import styles from "./ReviewView.module.css";

interface EvalBadge {
  score: number | null;
  evaluating: boolean;
}

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
  const [reviewFilter, setReviewFilter] = useState<ReviewFilter>("all");

  // FM-11: Evaluator state
  const [evalResult, setEvalResult] = useState<EvaluationResult | null>(null);
  const [annotations, setAnnotations] = useState<AnnotationInfo[]>([]);
  const [fileScores, setFileScores] = useState<Record<string, number>>({});
  const [evalBadges, setEvalBadges] = useState<Record<string, EvalBadge>>({});
  const [evaluatingCurrent, setEvaluatingCurrent] = useState(false);

  const selectedAgentIdRef = useRef(selectedAgentId);
  selectedAgentIdRef.current = selectedAgentId;

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

  // Load agents + batch-fetch their eval badges
  useEffect(() => {
    if (!selectedMissionId) {
      setAgents([]);
      setSelectedAgentId(null);
      return;
    }

    commands.listAgentsByMission(selectedMissionId).then(async (list) => {
      setAgents(list);
      if (list.length > 0) {
        setSelectedAgentId(list[0].id);
      } else {
        setSelectedAgentId(null);
      }
      setDiffFiles([]);
      setSelectedFilePath(null);

      // Batch-load eval badges for all agents
      const badges: Record<string, EvalBadge> = {};
      for (const agent of list) {
        try {
          const r = await commands.getEvaluationResult(agent.id);
          badges[agent.id] = {
            score: r?.overall_score ?? null,
            evaluating: false,
          };
        } catch {
          badges[agent.id] = { score: null, evaluating: false };
        }
      }
      setEvalBadges(badges);
    });
  }, [selectedMissionId]);

  // Listen for evaluation-complete events (all agents, not just selected)
  useEffect(() => {
    const unlisten = onEvaluationComplete((payload) => {
      // Update badge for that agent
      setEvalBadges((prev) => ({
        ...prev,
        [payload.agent_id]: { score: payload.overall_score, evaluating: false },
      }));

      // If this is the currently selected agent, refresh its details
      if (payload.agent_id === selectedAgentIdRef.current) {
        setEvaluatingCurrent(false);
        loadEvaluation(payload.agent_id);
      }
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  const loadEvaluation = useCallback(async (agentId: string) => {
    try {
      const result = await commands.getEvaluationResult(agentId);
      setEvalResult(result);

      if (result) {
        const anns = await commands.getAnnotations({ agent_id: agentId });
        setAnnotations(anns);

        const scores: Record<string, number> = {};
        const fileAnnotations = new Map<string, AnnotationInfo[]>();
        for (const ann of anns) {
          const existing = fileAnnotations.get(ann.file_path) ?? [];
          existing.push(ann);
          fileAnnotations.set(ann.file_path, existing);
        }

        for (const [filePath, fileAnns] of fileAnnotations) {
          const errorCount = fileAnns.filter((a) => a.severity === "error").length;
          const warningCount = fileAnns.filter((a) => a.severity === "warning").length;
          const penalty = errorCount * 2 + warningCount * 0.5;
          scores[filePath] = Math.max(0, Math.min(10, result.overall_score - penalty));
        }

        setFileScores(scores);
      } else {
        setAnnotations([]);
        setFileScores({});
      }
    } catch {
      setEvalResult(null);
      setAnnotations([]);
      setFileScores({});
    }
  }, []);

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

    await loadEvaluation(agentId);

    // Check if badge shows evaluating for this agent
    setEvaluatingCurrent((prev) => {
      const badge = evalBadges[agentId];
      return badge?.evaluating ?? prev;
    });
  }, [loadEvaluation, evalBadges]);

  useEffect(() => {
    if (selectedAgentId) {
      loadDiff(selectedAgentId);
    }
  }, [selectedAgentId, loadDiff]);

  const handleTriggerEvaluation = useCallback(async () => {
    if (!selectedAgentId) return;
    setEvaluatingCurrent(true);
    setEvalBadges((prev) => ({
      ...prev,
      [selectedAgentId]: { score: null, evaluating: true },
    }));
    try {
      await commands.triggerEvaluation(selectedAgentId);
    } catch (e) {
      setError(String(e));
      setEvaluatingCurrent(false);
      setEvalBadges((prev) => ({
        ...prev,
        [selectedAgentId]: { score: null, evaluating: false },
      }));
    }
  }, [selectedAgentId]);

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

  const handleApproveAll = useCallback(async () => {
    if (!selectedMissionId) return;
    for (const agent of agents) {
      if (reviewStatuses[agent.id] !== "approved") {
        try {
          await commands.submitReviewAction({
            agent_id: agent.id,
            action: "approved",
          });
          setReviewStatuses((prev) => ({ ...prev, [agent.id]: "approved" }));
        } catch {}
      }
    }
  }, [selectedMissionId, agents, reviewStatuses]);

  const handleMergeAll = useCallback(() => {
    alert("Merge All is not yet implemented.");
  }, []);

  const handleAnnotationStatusChange = useCallback(
    (id: string, newStatus: string) => {
      setAnnotations((prev) =>
        prev.map((a) => (a.id === id ? { ...a, status: newStatus as AnnotationInfo["status"] } : a)),
      );
      if (selectedAgentId) {
        commands.getEvaluationResult(selectedAgentId).then((r) => setEvalResult(r));
      }
    },
    [selectedAgentId],
  );

  const filteredAgents = agents.filter((a) => {
    if (reviewFilter === "all") return true;
    if (reviewFilter === "needs_review") return reviewStatuses[a.id] !== "approved";
    if (reviewFilter === "approved") return reviewStatuses[a.id] === "approved";
    return true;
  });

  const selectedFile = diffFiles.find((f) => f.path === selectedFilePath) ?? null;
  const fileAnnotations = selectedFilePath
    ? annotations.filter((a) => a.file_path === selectedFilePath)
    : [];
  const currentFileScore = selectedFilePath ? (fileScores[selectedFilePath] ?? null) : null;

  const selectedAgent = agents.find((a) => a.id === selectedAgentId);
  const canTriggerEval =
    selectedAgent?.status === "completed" && !evalResult && !evaluatingCurrent;

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

      {(evalResult || evaluatingCurrent || canTriggerEval) && (
        <EvalSummaryBar
          result={evalResult}
          evaluating={evaluatingCurrent}
          onTrigger={handleTriggerEvaluation}
          canTrigger={canTriggerEval}
        />
      )}

      <ReviewFilterBar
        filter={reviewFilter}
        onFilterChange={setReviewFilter}
        agents={agents}
        reviewStatuses={reviewStatuses}
        totalFiles={diffFiles.length}
        onApproveAll={handleApproveAll}
        onMergeAll={handleMergeAll}
      />

      <div className={styles.body}>
        <AgentReviewTabs
          agents={filteredAgents}
          selectedAgentId={selectedAgentId}
          reviewStatuses={reviewStatuses}
          evalBadges={evalBadges}
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
              fileScores={fileScores}
            />
            <DiffViewer
              file={selectedFile}
              theme={editorTheme}
              annotations={fileAnnotations}
              fileScore={currentFileScore}
              onAnnotationStatusChange={handleAnnotationStatusChange}
            />
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
