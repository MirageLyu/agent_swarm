import { useTranslation } from "react-i18next";
import type { MissionReportCostBreakdown } from "../../ipc/commands";
import styles from "./CostBreakdownSection.module.css";

interface Props {
  breakdown: MissionReportCostBreakdown;
}

/**
 * FM-12 FR-07: Cost Breakdown 节
 * 顶部预算条 + 双列：By Model / By Task。
 * 实测 SVG 折线图等高级可视化留给 FM-13 dashboard，这里维持文本表格 + 横向条。
 */
export function CostBreakdownSection({ breakdown }: Props) {
  const { t } = useTranslation("report");
  const showBudget = breakdown.budget_usd !== null && breakdown.budget_usd > 0;
  const usedRatio = breakdown.budget_used_ratio ?? 0;
  const ratioPct = Math.min(100, usedRatio * 100);
  const budgetTone = ratioPct >= 80 ? "bad" : ratioPct >= 50 ? "warn" : "ok";

  // 用最大成本作为 100%，画横向条
  const maxModelCost = Math.max(0, ...breakdown.by_model.map((m) => m.cost_usd));
  const maxTaskCost = Math.max(0, ...breakdown.by_task.map((t) => t.cost_usd));

  return (
    <div className={styles.container}>
      <div className={styles.totalRow}>
        <div>
          <div className={styles.totalLabel}>{t("costTotal")}</div>
          <div className={styles.totalValue}>${breakdown.total_usd.toFixed(4)}</div>
        </div>
        <div className={styles.tokenStats}>
          <span>{breakdown.total_input_tokens.toLocaleString()} {t("costInShort")}</span>
          <span>·</span>
          <span>{breakdown.total_output_tokens.toLocaleString()} {t("costOutShort")}</span>
        </div>
      </div>

      {showBudget && breakdown.budget_usd !== null && (
        <div className={styles.budgetBlock}>
          <div className={styles.budgetTopRow}>
            <span>{t("costBudgetUsage")}</span>
            <span className={styles[`budget_${budgetTone}`]}>
              ${breakdown.total_usd.toFixed(2)} / ${breakdown.budget_usd.toFixed(2)}
              {" "}({ratioPct.toFixed(0)}%)
            </span>
          </div>
          <div className={styles.budgetBar}>
            <div
              className={`${styles.budgetFill} ${styles[`budgetFill_${budgetTone}`]}`}
              style={{ width: `${ratioPct}%` }}
            />
          </div>
        </div>
      )}

      <div className={styles.splitGrid}>
        <div className={styles.col}>
          <h4 className={styles.colTitle}>{t("costByModel")}</h4>
          {breakdown.by_model.length === 0 ? (
            <p className={styles.empty}>{t("costNoneRecorded")}</p>
          ) : (
            <ul className={styles.barList}>
              {breakdown.by_model.map((m) => (
                <li key={m.model} className={styles.barItem}>
                  <div className={styles.barRow}>
                    <span className={styles.barLabel}>{m.model}</span>
                    <span className={styles.barValue}>${m.cost_usd.toFixed(4)}</span>
                  </div>
                  <div className={styles.barTrack}>
                    <div
                      className={styles.barFill}
                      style={{
                        width: maxModelCost > 0 ? `${(m.cost_usd / maxModelCost) * 100}%` : "0%",
                      }}
                    />
                  </div>
                  <div className={styles.barSubLabel}>
                    {m.input_tokens.toLocaleString()} {t("costInShort")} · {m.output_tokens.toLocaleString()} {t("costOutShort")}
                  </div>
                </li>
              ))}
            </ul>
          )}
        </div>

        <div className={styles.col}>
          <h4 className={styles.colTitle}>{t("costByTask")}</h4>
          {breakdown.by_task.length === 0 ? (
            <p className={styles.empty}>{t("costNoTasks")}</p>
          ) : (
            <ul className={styles.barList}>
              {breakdown.by_task.map((t) => (
                <li key={t.task_id} className={styles.barItem}>
                  <div className={styles.barRow}>
                    <span className={styles.barLabel} title={t.title}>
                      {t.title}
                    </span>
                    <span className={styles.barValue}>${t.cost_usd.toFixed(4)}</span>
                  </div>
                  <div className={styles.barTrack}>
                    <div
                      className={styles.barFill}
                      style={{
                        width: maxTaskCost > 0 ? `${(t.cost_usd / maxTaskCost) * 100}%` : "0%",
                      }}
                    />
                  </div>
                </li>
              ))}
            </ul>
          )}
        </div>
      </div>
    </div>
  );
}
