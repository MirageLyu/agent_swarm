import { useEffect, useState, useRef, useCallback } from "react";
import { commands } from "../ipc";
import type { SchedulerStatus, MissionCostSummary } from "../ipc/commands";
import { useTaskStore } from "../stores/task-store";
import styles from "./TopBarMetrics.module.css";

const BUDGET = 30;
const REFRESH_INTERVAL = 3000;

function formatDuration(seconds: number): string {
  const m = Math.floor(seconds / 60);
  const s = seconds % 60;
  return `${m}m ${s}s`;
}

function costClass(cost: number): string {
  const ratio = cost / BUDGET;
  if (ratio >= 0.8) return styles.costDanger;
  if (ratio >= 0.5) return styles.costWarning;
  return styles.costNormal;
}

export function TopBarMetrics() {
  const [scheduler, setScheduler] = useState<SchedulerStatus | null>(null);
  const [cost, setCost] = useState<MissionCostSummary | null>(null);
  const [elapsed, setElapsed] = useState(0);
  const timerRef = useRef<ReturnType<typeof setInterval>>(undefined);
  const startTimeRef = useRef<number>(0);

  const missions = useTaskStore((s) => s.missions);
  const activeMission = missions.find(
    (m) => m.status === "running",
  );

  const fetchData = useCallback(async () => {
    try {
      const s = await commands.getSchedulerStatus();
      setScheduler(s);
    } catch {}

    if (activeMission) {
      try {
        const c = await commands.getMissionCostSummary(activeMission.id);
        setCost(c);
      } catch {}
    }
  }, [activeMission]);

  useEffect(() => {
    fetchData();
    const id = setInterval(fetchData, REFRESH_INTERVAL);
    return () => clearInterval(id);
  }, [fetchData]);

  useEffect(() => {
    if (activeMission) {
      if (!startTimeRef.current) {
        startTimeRef.current = Date.now();
      }
      timerRef.current = setInterval(() => {
        setElapsed(Math.floor((Date.now() - startTimeRef.current) / 1000));
      }, 1000);
      return () => clearInterval(timerRef.current);
    } else {
      startTimeRef.current = 0;
      setElapsed(0);
    }
  }, [activeMission]);

  const isActive = !!(scheduler && (scheduler.active_agents > 0 || activeMission));

  if (!isActive) return null;

  const totalAgents =
    scheduler ? scheduler.active_agents + scheduler.ready_tasks + scheduler.blocked_tasks : 0;
  const activeAgents = scheduler?.active_agents ?? 0;
  const totalCost = cost?.total_cost ?? 0;

  return (
    <div className={styles.metrics}>
      <div className={styles.badge}>
        <div className={styles.dot} />
        Agents: <strong>{activeAgents}/{totalAgents}</strong>
      </div>
      <div className={styles.badge}>
        <svg width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5">
          <circle cx="8" cy="8" r="6" />
          <path d="M8 5v3.5l2.5 1.5" />
        </svg>
        {formatDuration(elapsed)}
      </div>
      <div className={`${styles.badge} ${costClass(totalCost)}`}>
        <span>$</span>
        <strong>{totalCost.toFixed(2)}</strong> / ${BUDGET}
      </div>
    </div>
  );
}
