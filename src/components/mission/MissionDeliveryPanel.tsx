import { useCallback, useState } from "react";
import { Button } from "../ui";
import { commands } from "../../ipc/commands";
import { useUiStore } from "../../stores/ui-store";
import type { MissionDeliveredPayload } from "../../ipc/events";
import styles from "./MissionDeliveryPanel.module.css";

interface MissionDeliveryPanelProps {
  payload: MissionDeliveredPayload;
}

/**
 * FM-15 v2.2 P4-S4: Mission 完成后的交付面板。
 * 当 mission-delivered 事件触发时，MissionsView 会传入 payload 并渲染本面板。
 *
 * 包含：
 * - 摘要：总任务数 / 总 commit 数 / artifact 数
 * - 一键打开（编辑器、终端、Finder）
 * - 已发布 artifact 列表（含 file paths + summary）
 * - LLM 解决冲突提醒（FR-14.2 "⚠ AI 解决，建议复核"）
 * - auto-resolved 文件提醒
 */
export function MissionDeliveryPanel({ payload }: MissionDeliveryPanelProps) {
  const [busyKey, setBusyKey] = useState<string | null>(null);
  const openMissionReport = useUiStore((s) => s.openMissionReport);

  const handleOpen = useCallback(
    async (action: "editor" | "terminal" | "finder") => {
      setBusyKey(action);
      try {
        if (action === "editor") {
          await commands.openInEditor(payload.repoPath);
        } else if (action === "terminal") {
          await commands.openInTerminal(payload.repoPath);
        } else {
          await commands.openInFinder(payload.repoPath);
        }
      } catch (err) {
        console.error(`[delivery] open ${action} failed`, err);
      } finally {
        setBusyKey(null);
      }
    },
    [payload.repoPath],
  );

  return (
    <div className={styles.container}>
      <header className={styles.header}>
        <div className={styles.title}>
          <span className={styles.checkmark} aria-hidden>✓</span>
          Mission Delivered
        </div>
      </header>

      <div className={styles.summary}>
        <div className={styles.summaryItem}>
          <span className={styles.summaryLabel}>Tasks</span>
          <span className={styles.summaryValue}>{payload.totalTasks}</span>
        </div>
        <div className={styles.summaryItem}>
          <span className={styles.summaryLabel}>Commits on {payload.mainBranch}</span>
          <span className={styles.summaryValue}>{payload.totalCommits}</span>
        </div>
        <div className={styles.summaryItem}>
          <span className={styles.summaryLabel}>Artifacts</span>
          <span className={styles.summaryValue}>{payload.artifacts.length}</span>
        </div>
      </div>

      <div className={styles.repoPath}>
        <span className={styles.repoPathText} title={payload.repoPath}>
          {payload.repoPath}
        </span>
      </div>

      <div className={styles.actions}>
        <Button
          variant="primary"
          size="sm"
          onClick={() => handleOpen("editor")}
          disabled={busyKey !== null}
        >
          {busyKey === "editor" ? "Opening\u2026" : "Open in Editor"}
        </Button>
        <Button
          variant="secondary"
          size="sm"
          onClick={() => handleOpen("terminal")}
          disabled={busyKey !== null}
        >
          {busyKey === "terminal" ? "Opening\u2026" : "Open Terminal"}
        </Button>
        <Button
          variant="secondary"
          size="sm"
          onClick={() => handleOpen("finder")}
          disabled={busyKey !== null}
        >
          {busyKey === "finder" ? "Opening\u2026" : "Reveal in Finder"}
        </Button>
        <Button
          variant="primary"
          size="sm"
          onClick={() => openMissionReport(payload.missionId)}
          title="Open the full mission report"
        >
          View Full Report
        </Button>
      </div>

      {payload.llmResolvedFiles.length > 0 ? (
        <div className={styles.warningBlock}>
          <span className={styles.warningTitle}>
            ⚠ {payload.llmResolvedFiles.length} file{payload.llmResolvedFiles.length === 1 ? "" : "s"} resolved by AI — please review
          </span>
          <ul className={styles.warningList}>
            {payload.llmResolvedFiles.map((p) => (
              <li key={p}>{p}</li>
            ))}
          </ul>
        </div>
      ) : null}

      {payload.autoResolvedFiles.length > 0 ? (
        <div className={styles.warningBlock}>
          <span className={styles.warningTitle}>
            ⚠ {payload.autoResolvedFiles.length} file{payload.autoResolvedFiles.length === 1 ? "" : "s"} auto-merged (theirs / heuristic) — verify if needed
          </span>
          <ul className={styles.warningList}>
            {payload.autoResolvedFiles.slice(0, 8).map((p) => (
              <li key={p}>{p}</li>
            ))}
            {payload.autoResolvedFiles.length > 8 ? (
              <li>… and {payload.autoResolvedFiles.length - 8} more</li>
            ) : null}
          </ul>
        </div>
      ) : null}

      <div className={styles.section}>
        <div className={styles.sectionTitle}>
          Published Artifacts
          <span className={styles.sectionCount}>({payload.artifacts.length})</span>
        </div>
        {payload.artifacts.length === 0 ? (
          <div className={styles.empty}>No artifacts were published.</div>
        ) : (
          <div className={styles.artifactList}>
            {payload.artifacts.map((a) => (
              <div className={styles.artifactItem} key={`${a.taskId}.${a.localName}`}>
                <div className={styles.artifactHeader}>
                  <span className={styles.artifactName}>{a.localName}</span>
                  <span className={styles.artifactType}>{a.artifactType}</span>
                  <span className={styles.artifactTask}>from "{a.taskTitle}"</span>
                </div>
                {a.summary ? (
                  <div className={styles.artifactSummary}>{a.summary}</div>
                ) : null}
                {a.filePaths.length > 0 ? (
                  <div className={styles.artifactPaths}>
                    {a.filePaths.map((p) => (
                      <span className={styles.artifactPath} key={p}>{p}</span>
                    ))}
                  </div>
                ) : null}
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
