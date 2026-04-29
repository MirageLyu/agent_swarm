/**
 * FM-13 Lite — Insights view.
 *
 * 完整版 FM-13 的范围（成本/质量/效率/异常 4 panel + Live 轮询 + 甘特图 + 环形进度）
 * 是 5-7 天工作量。MVP lite 只做"对开发者最有 actionable 价值"的两个：
 *  - Cost Trend：跨 mission 的成本走势 + 模型拆分
 *  - Anomalies：自动检测的异常（cost spike / long running / failed agent）
 *
 * 设计取舍：
 *  - 一次性加载 + 手动 Refresh 按钮，不做 3 秒轮询（避免 SQLite 在 mission 多时常驻 CPU）
 *  - SVG 折线图自绘，不引依赖
 *  - 异常点击直接跳到对应 mission 的 ReportView（如果 mission 已完成）或 MissionsView
 */
import { useCallback, useEffect, useMemo, useState } from "react";
import { Button } from "../components/ui/Button";
import { commands, type Anomaly, type MissionCostPoint } from "../ipc/commands";
import { useUiStore } from "../stores/ui-store";
import styles from "./InsightsView.module.css";

export function InsightsView() {
  const [trend, setTrend] = useState<MissionCostPoint[] | null>(null);
  const [anomalies, setAnomalies] = useState<Anomaly[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [t, a] = await Promise.all([
        commands.getCostTrend(50),
        commands.getAnomalies(null),
      ]);
      setTrend(t);
      setAnomalies(a);
    } catch (e) {
      setError(`Failed to load insights: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  return (
    <div className={styles.container}>
      <div className={styles.header}>
        <div>
          <h2 className={styles.title}>Insights</h2>
          <p className={styles.subtitle}>
            Lightweight dashboard for cost trend and runtime anomalies. Updated
            on demand from your local SQLite.
          </p>
        </div>
        <Button variant="secondary" onClick={load} disabled={loading}>
          {loading ? "Loading…" : "Refresh"}
        </Button>
      </div>

      {error && <div className={styles.error}>{error}</div>}

      <div className={styles.panels}>
        <CostTrendPanel data={trend} loading={loading} />
        <AnomaliesPanel data={anomalies} loading={loading} />
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Cost Trend Panel
// ---------------------------------------------------------------------------

function CostTrendPanel({
  data,
  loading,
}: {
  data: MissionCostPoint[] | null;
  loading: boolean;
}) {
  const aggregate = useMemo(() => aggregateModelCosts(data ?? []), [data]);

  if (loading && !data) {
    return (
      <section className={styles.panel}>
        <h3 className={styles.panelTitle}>Cost Trend</h3>
        <div className={styles.empty}>Loading…</div>
      </section>
    );
  }

  if (!data || data.length === 0) {
    return (
      <section className={styles.panel}>
        <h3 className={styles.panelTitle}>Cost Trend</h3>
        <div className={styles.empty}>
          No cost data yet. Run a mission to start tracking.
        </div>
      </section>
    );
  }

  const totalCost = data.reduce((s, p) => s + p.total_cost, 0);
  const totalTokens = data.reduce(
    (s, p) => s + p.total_input_tokens + p.total_output_tokens,
    0,
  );
  const maxCost = Math.max(...data.map((p) => p.total_cost), 0.001);

  return (
    <section className={styles.panel}>
      <h3 className={styles.panelTitle}>Cost Trend</h3>
      <div className={styles.metricRow}>
        <Metric label="Missions" value={String(data.length)} />
        <Metric label="Total Cost" value={`$${totalCost.toFixed(4)}`} />
        <Metric label="Tokens" value={formatTokens(totalTokens)} />
      </div>

      <div className={styles.subSection}>
        <div className={styles.subTitle}>Cost per Mission</div>
        <CostSparkline data={data} maxCost={maxCost} />
      </div>

      {aggregate.length > 0 && (
        <div className={styles.subSection}>
          <div className={styles.subTitle}>By Model</div>
          <table className={styles.modelTable}>
            <thead>
              <tr>
                <th>Model</th>
                <th>Cost</th>
                <th>Tokens</th>
                <th>%</th>
              </tr>
            </thead>
            <tbody>
              {aggregate.map((m) => (
                <tr key={m.model}>
                  <td>{m.model}</td>
                  <td>${m.cost.toFixed(4)}</td>
                  <td>{formatTokens(m.tokens)}</td>
                  <td>{totalCost > 0 ? ((m.cost / totalCost) * 100).toFixed(1) : "0"}%</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}

function CostSparkline({
  data,
  maxCost,
}: {
  data: MissionCostPoint[];
  maxCost: number;
}) {
  const width = 600;
  const height = 120;
  const padding = { top: 10, right: 10, bottom: 24, left: 40 };
  const innerW = width - padding.left - padding.right;
  const innerH = height - padding.top - padding.bottom;

  if (data.length === 0) return null;

  // X 轴：mission index 等距；Y 轴：cost 0 → maxCost
  const stepX = data.length > 1 ? innerW / (data.length - 1) : innerW;
  const points = data.map((p, i) => {
    const x = padding.left + (data.length === 1 ? innerW / 2 : i * stepX);
    const y = padding.top + innerH - (p.total_cost / maxCost) * innerH;
    return { x, y, p };
  });

  const linePath = points
    .map((pt, i) => `${i === 0 ? "M" : "L"} ${pt.x.toFixed(1)} ${pt.y.toFixed(1)}`)
    .join(" ");

  return (
    <svg
      className={styles.sparkline}
      viewBox={`0 0 ${width} ${height}`}
      preserveAspectRatio="none"
      role="img"
      aria-label="Cost per mission sparkline"
    >
      {/* y-axis labels */}
      <text x={4} y={padding.top + 4} className={styles.svgAxisLabel}>
        ${maxCost.toFixed(2)}
      </text>
      <text x={4} y={height - padding.bottom + 4} className={styles.svgAxisLabel}>
        $0
      </text>
      {/* baseline */}
      <line
        x1={padding.left}
        y1={height - padding.bottom}
        x2={width - padding.right}
        y2={height - padding.bottom}
        className={styles.svgAxis}
      />
      {/* polyline */}
      <path d={linePath} className={styles.svgLine} fill="none" />
      {/* dots with title tooltips */}
      {points.map((pt) => (
        <g key={pt.p.mission_id}>
          <circle cx={pt.x} cy={pt.y} r={3} className={styles.svgDot} />
          <title>
            {pt.p.mission_title} — ${pt.p.total_cost.toFixed(4)} (
            {pt.p.created_at.slice(0, 10)})
          </title>
        </g>
      ))}
    </svg>
  );
}

function aggregateModelCosts(points: MissionCostPoint[]) {
  const map = new Map<string, { cost: number; tokens: number }>();
  for (const p of points) {
    for (const m of p.model_breakdown) {
      const cur = map.get(m.model) ?? { cost: 0, tokens: 0 };
      cur.cost += m.cost;
      cur.tokens += m.tokens;
      map.set(m.model, cur);
    }
  }
  return Array.from(map.entries())
    .map(([model, v]) => ({ model, ...v }))
    .sort((a, b) => b.cost - a.cost);
}

// ---------------------------------------------------------------------------
// Anomalies Panel
// ---------------------------------------------------------------------------

function AnomaliesPanel({
  data,
  loading,
}: {
  data: Anomaly[] | null;
  loading: boolean;
}) {
  const openMissionReport = useUiStore((s) => s.openMissionReport);

  if (loading && !data) {
    return (
      <section className={styles.panel}>
        <h3 className={styles.panelTitle}>Anomalies</h3>
        <div className={styles.empty}>Loading…</div>
      </section>
    );
  }

  if (!data || data.length === 0) {
    return (
      <section className={styles.panel}>
        <h3 className={styles.panelTitle}>
          Anomalies <span className={styles.allClear}>All Clear</span>
        </h3>
        <div className={styles.empty}>
          No cost spikes, long-running agents, or recent failures detected.
        </div>
      </section>
    );
  }

  const counts = countBySeverity(data);

  return (
    <section className={styles.panel}>
      <h3 className={styles.panelTitle}>
        Anomalies{" "}
        <span className={styles.severityCount}>
          {counts.critical > 0 && (
            <span className={`${styles.badge} ${styles.badgeCritical}`}>
              {counts.critical} critical
            </span>
          )}
          {counts.warn > 0 && (
            <span className={`${styles.badge} ${styles.badgeWarn}`}>
              {counts.warn} warn
            </span>
          )}
          {counts.info > 0 && (
            <span className={`${styles.badge} ${styles.badgeInfo}`}>
              {counts.info} info
            </span>
          )}
        </span>
      </h3>

      <ul className={styles.anomalyList}>
        {data.map((a, i) => (
          <li key={`${a.mission_id}-${a.agent_id}-${i}`} className={styles.anomalyItem}>
            <div className={styles.anomalyTop}>
              <span
                className={`${styles.kindBadge} ${kindBadgeClass(a.kind, styles)}`}
              >
                {kindLabel(a.kind)}
              </span>
              <span className={`${styles.badge} ${severityBadgeClass(a.severity, styles)}`}>
                {a.severity}
              </span>
              <span className={styles.anomalyTime}>{a.occurred_at.slice(0, 19).replace("T", " ")}</span>
            </div>
            <div className={styles.anomalyMessage}>{a.message}</div>
            <div className={styles.anomalyMeta}>
              <button
                className={styles.linkButton}
                onClick={() => openMissionReport(a.mission_id)}
                title="Open mission report"
              >
                {a.mission_title}
              </button>
              {a.task_title && <span> · {a.task_title}</span>}
            </div>
          </li>
        ))}
      </ul>
    </section>
  );
}

function countBySeverity(items: Anomaly[]) {
  const c = { critical: 0, warn: 0, info: 0 };
  for (const it of items) {
    c[it.severity] += 1;
  }
  return c;
}

function kindLabel(kind: Anomaly["kind"]): string {
  switch (kind) {
    case "cost_spike":
      return "Cost Spike";
    case "long_running":
      return "Long Running";
    case "failed_agent":
      return "Failed Agent";
  }
}

function kindBadgeClass(
  kind: Anomaly["kind"],
  s: Record<string, string>,
): string {
  switch (kind) {
    case "cost_spike":
      return s.kindCost ?? "";
    case "long_running":
      return s.kindLong ?? "";
    case "failed_agent":
      return s.kindFailed ?? "";
  }
}

function severityBadgeClass(
  sev: Anomaly["severity"],
  s: Record<string, string>,
): string {
  switch (sev) {
    case "critical":
      return s.badgeCritical ?? "";
    case "warn":
      return s.badgeWarn ?? "";
    case "info":
      return s.badgeInfo ?? "";
  }
}

// ---------------------------------------------------------------------------
// Misc helpers
// ---------------------------------------------------------------------------

function Metric({ label, value }: { label: string; value: string }) {
  return (
    <div className={styles.metric}>
      <div className={styles.metricValue}>{value}</div>
      <div className={styles.metricLabel}>{label}</div>
    </div>
  );
}

function formatTokens(n: number): string {
  if (n < 1000) return String(n);
  if (n < 1_000_000) return `${(n / 1000).toFixed(1)}k`;
  return `${(n / 1_000_000).toFixed(2)}M`;
}
