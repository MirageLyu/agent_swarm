/**
 * FM-15 v2.2 Slice 1: 裸 Planner Agent Loop 面板。
 *
 * 仅用于打通端到端：把 `planner-step` 事件按时间顺序文本化展示。
 * 不做语义高亮、不做 DAG 增量渲染——这些留给 S4 的正式 UI 升级。
 *
 * 用法：当 `planMission` 返回 `planner_session_id` 时挂载，
 * 内部订阅 `onPlannerStep` 并按 step_no 增量追加。
 */
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { onPlannerStep, type PlannerStepPayload } from "../../ipc/events";
import { commands, type PlannerStepRow } from "../../ipc/commands";

interface PlannerLoopPanelProps {
  /**
   * 已知的 planner session id；省略时面板会从第一个 planner-step 事件里自动发现，
   * 用于 sign_contract / 异步 planner 启动场景（前端发起请求后立即挂载，session_id
   * 直到 plan 完成才返回）。
   */
  sessionId?: string;
  /** 流式期间持续追加；完成后转静态。可省略，仅用于外部信号。 */
  isLive?: boolean;
  /** 标题里的可选前缀（"Planner Agent Loop" 默认；Pre-flight 用 "Pre-flight planner"）。 */
  label?: string;
}

interface DisplayStep {
  step_no: number;
  kind: string;
  tool_name?: string;
  tool_args?: string;
  tool_result?: string;
  text_content?: string;
  error?: string;
}

function payloadToStep(p: PlannerStepPayload): DisplayStep {
  return {
    step_no: p.step_no,
    kind: p.kind,
    tool_name: p.tool_name,
    tool_args: p.tool_args,
    tool_result: p.tool_result,
    text_content: p.text_content,
    error: p.error,
  };
}

function rowToStep(r: PlannerStepRow): DisplayStep {
  return {
    step_no: r.step_no,
    kind: r.kind,
    tool_name: r.tool_name ?? undefined,
    tool_args: r.tool_args ?? undefined,
    tool_result: r.tool_result ?? undefined,
    text_content: r.text_content ?? undefined,
  };
}

const KIND_BADGE: Record<string, { bg: string; fg: string; label: string }> = {
  tool_call: { bg: "#dbeafe", fg: "#1d4ed8", label: "TOOL CALL" },
  tool_result: { bg: "#dcfce7", fg: "#166534", label: "TOOL OK" },
  tool_result_error: { bg: "#fee2e2", fg: "#b91c1c", label: "TOOL ERR" },
  text: { bg: "#f3f4f6", fg: "#374151", label: "TEXT" },
  thinking: { bg: "#ede9fe", fg: "#6d28d9", label: "THINKING" },
  error: { bg: "#fee2e2", fg: "#b91c1c", label: "ERROR" },
  status: { bg: "#fef3c7", fg: "#92400e", label: "STATUS" },
};

export function PlannerLoopPanel({
  sessionId,
  isLive = true,
  label,
}: PlannerLoopPanelProps) {
  const { t } = useTranslation("mission");
  const resolvedLabel = label ?? t("planner.label");
  const [steps, setSteps] = useState<DisplayStep[]>([]);
  const [discoveredSessionId, setDiscoveredSessionId] = useState<string | null>(
    sessionId ?? null,
  );
  const seenRef = useRef<Set<number>>(new Set());
  const containerRef = useRef<HTMLDivElement>(null);

  // 当传入的 sessionId 变化时同步 internal discovered 值
  useEffect(() => {
    if (sessionId) setDiscoveredSessionId(sessionId);
  }, [sessionId]);

  useEffect(() => {
    seenRef.current = new Set();
    setSteps([]);
    let cancelled = false;

    // 已知 sessionId：先拉历史
    if (sessionId) {
      commands
        .listPlannerSteps(sessionId)
        .then((rows) => {
          if (cancelled) return;
          const initial = rows.map(rowToStep);
          seenRef.current = new Set(initial.map((s) => s.step_no));
          setSteps(initial);
        })
        .catch((e) => console.warn("listPlannerSteps failed:", e));
    }

    // 订阅实时 step 事件
    const unsubP = onPlannerStep((p) => {
      // sessionId 已知 → 严格过滤；未知 → 锁定第一个看到的 session
      if (sessionId) {
        if (p.session_id !== sessionId) return;
      } else {
        setDiscoveredSessionId((prev) => prev ?? p.session_id);
        if (discoveredSessionId && p.session_id !== discoveredSessionId) return;
      }
      if (seenRef.current.has(p.step_no)) return;
      seenRef.current.add(p.step_no);
      setSteps((prev) => [...prev, payloadToStep(p)].sort((a, b) => a.step_no - b.step_no));
    });

    return () => {
      cancelled = true;
      unsubP.then((fn) => fn());
    };
    // discoveredSessionId 故意不放进依赖：useState setter 已经处理"锁定第一个"的逻辑，
    // 加进依赖反而会导致每次 first event 之后整个 useEffect 重跑、丢历史。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessionId]);

  useEffect(() => {
    if (!isLive) return;
    if (containerRef.current) {
      containerRef.current.scrollTop = containerRef.current.scrollHeight;
    }
  }, [steps, isLive]);

  return (
    <div
      style={{
        border: "1px solid var(--color-border)",
        borderRadius: 8,
        background: "var(--color-bg-elevated)",
        padding: 12,
        marginTop: 12,
        maxHeight: 360,
        overflow: "auto",
        fontSize: 12,
      }}
      ref={containerRef}
    >
      <div style={{ fontWeight: 600, marginBottom: 8, color: "var(--color-text-muted)" }}>
        {resolvedLabel}
        {discoveredSessionId
          ? ` · ${t("planner.session", { id: discoveredSessionId.slice(0, 8) })}`
          : ` · ${t("planner.waitingSession")}`}
        {" · "}
        {t("planner.stepCount", { count: steps.length })}
      </div>
      {steps.length === 0 && (
        <div style={{ color: "var(--color-text-muted)", fontStyle: "italic" }}>
          {t("planner.waitingSteps")}
        </div>
      )}
      {steps.map((s) => {
        const badge = KIND_BADGE[s.kind] ?? { bg: "#e5e7eb", fg: "#1f2937", label: s.kind };
        return (
          <div key={s.step_no} style={{ marginBottom: 6 }}>
            <span
              style={{
                display: "inline-block",
                background: badge.bg,
                color: badge.fg,
                borderRadius: 4,
                padding: "1px 6px",
                fontFamily: "var(--font-mono)",
                fontSize: 10,
                fontWeight: 600,
                marginRight: 6,
              }}
            >
              #{s.step_no} {badge.label}
            </span>
            {s.tool_name && (
              <span style={{ fontFamily: "var(--font-mono)", color: "#1d4ed8" }}>
                {s.tool_name}
              </span>
            )}
            {s.tool_args && (
              <pre
                style={{
                  margin: "4px 0 0 0",
                  padding: 6,
                  background: "rgba(0,0,0,0.04)",
                  borderRadius: 4,
                  whiteSpace: "pre-wrap",
                  wordBreak: "break-word",
                  fontSize: 11,
                }}
              >
                {t("planner.argsPrefix")}: {s.tool_args}
              </pre>
            )}
            {s.tool_result && (
              <pre
                style={{
                  margin: "4px 0 0 0",
                  padding: 6,
                  background: "rgba(0,0,0,0.04)",
                  borderRadius: 4,
                  whiteSpace: "pre-wrap",
                  wordBreak: "break-word",
                  fontSize: 11,
                  maxHeight: 200,
                  overflow: "auto",
                }}
              >
                {s.tool_result}
              </pre>
            )}
            {s.text_content && (
              <div style={{ marginTop: 4, whiteSpace: "pre-wrap", color: "var(--color-text)" }}>
                {s.text_content}
              </div>
            )}
            {s.error && (
              <div style={{ marginTop: 4, color: "#b91c1c" }}>{t("planner.errorPrefix")}: {s.error}</div>
            )}
          </div>
        );
      })}
    </div>
  );
}
