import { memo, useMemo } from "react";
import type { AgentEvent } from "../../../stores/agent-store";
import styles from "./EventLine.module.css";

interface GuardrailReport {
  name?: string;
  passed?: boolean;
  detail?: string;
  message?: string;
}

function asReports(meta: unknown): GuardrailReport[] {
  if (!Array.isArray(meta)) return [];
  return meta.filter(
    (r): r is GuardrailReport => typeof r === "object" && r !== null,
  );
}

interface GuardrailLineProps {
  event: AgentEvent;
}

/// guardrail_pass / guardrail_fail / guardrail_summary 三种 kind 共用此 renderer。
/// meta 是 GuardrailReport[]，展示成 ✓/✗ + 名称 + 折叠 detail；fail 的 detail 默认露出。
export const GuardrailLine = memo(function GuardrailLine({ event }: GuardrailLineProps) {
  const reports = useMemo(() => asReports(event.meta), [event.meta]);
  const allPassed = event.kind === "guardrail_pass";

  if (reports.length === 0) {
    // meta 缺失时退化展示。content 是 reports JSON 串或 summary 文本。
    return (
      <div className={styles.row}>
        <span
          className={`${styles.icon} ${
            allPassed ? styles.iconOk : styles.iconError
          }`}
        >
          {allPassed ? "✓" : "✗"}
        </span>
        <div className={styles.body}>
          <div className={styles.guardrailHeader}>
            <span
              className={`${styles.badge} ${
                allPassed ? styles.badgeOk : styles.badgeError
              }`}
            >
              guardrail
            </span>
            <span>{event.content}</span>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className={styles.row}>
      <span
        className={`${styles.icon} ${
          allPassed ? styles.iconOk : styles.iconError
        }`}
      >
        {allPassed ? "✓" : "✗"}
      </span>
      <div className={styles.body}>
        <div className={styles.guardrailHeader}>
          <span
            className={`${styles.badge} ${
              allPassed ? styles.badgeOk : styles.badgeError
            }`}
          >
            guardrail {allPassed ? "passed" : "failed"}
          </span>
          <span>{reports.length} check{reports.length === 1 ? "" : "s"}</span>
        </div>
        <div className={styles.guardrailReports}>
          {reports.map((r, i) => {
            const passed = r.passed === true;
            const detail = r.detail ?? r.message;
            return (
              <div
                key={i}
                className={`${styles.guardrailReport} ${
                  passed ? styles.guardrailReportPass : styles.guardrailReportFail
                }`}
              >
                <span>{passed ? "✓" : "✗"}</span>
                <strong>{r.name ?? `check ${i + 1}`}</strong>
                {detail && <span className={styles.guardrailDetail}>{detail}</span>}
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
});
