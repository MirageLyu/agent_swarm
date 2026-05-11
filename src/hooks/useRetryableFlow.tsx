/**
 * 流程型交互的统一失败重试 hook。
 *
 * 用于 sign_contract / plan_mission / start_mission 等"多步骤可能失败、
 * 失败时不应让用户从头开始"的命令。详见 `.cursor/rules/retryable-flow.mdc`。
 *
 * 核心保证：
 * 1. 失败 banner 自动包含「重试 / 放弃」按钮，统一 UX；
 * 2. 错误信息走 `formatBackendError`（i18n + IpcError 解码）；
 * 3. 重试不重新初始化 ——`onSuccess` 只在最终成功时调用一次；
 * 4. 重试期间禁用放弃按钮，避免用户误操作创建脏状态。
 *
 * 与"普通 Promise + setError"的区别：调用方不需要每个 view 重写相同的
 * loading / error / retry 三件套。
 */
import { useCallback, useMemo, useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { formatBackendError } from "../i18n/format-error";
import styles from "./useRetryableFlow.module.css";

export type FlowState = "idle" | "running" | "success" | "error";

interface UseRetryableFlowOpts<T> {
  /**
   * 操作名（slug）。用于内部日志 + i18n 兜底文案的 key。
   * 例如 "sign_contract"，对应 `errors.flow.sign_contract.failureContext` 翻译。
   */
  operation: string;
  /** 真正发请求的函数。每次重试会重新调用。 */
  invoke: () => Promise<T>;
  /** 成功回调。`run` 多次但只有最后那次成功才会触发。 */
  onSuccess?: (result: T) => void;
  /**
   * 业务上明确不可重试时设 false（极少用，例如外部支付）。
   * 默认 true。failureBanner 会据此显示/隐藏重试按钮。
   */
  retryable?: boolean;
  /**
   * 用户点"放弃"按钮的回调。不传则默认只 reset 内部状态，
   * 不影响外部界面（适合"暂时关掉错误提示"的场景）。
   */
  onAbandon?: () => void;
}

export interface UseRetryableFlowReturn<T> {
  state: FlowState;
  result: T | null;
  /** 已格式化的错误信息（formatBackendError 输出）。仅 state==='error' 时有值。 */
  error: string | null;
  /** 累计调用次数（含失败重试）。便于排查。 */
  attempts: number;
  /** 触发流程：首次或失败后重试都用它。重复点击在 running 期间被忽略。 */
  run: () => void;
  /** 手工清除失败状态（如外部决定放弃）。 */
  reset: () => void;
  /**
   * 失败状态下的统一 banner（含错误信息 + 重试 + 放弃按钮）。
   * `state !== 'error'` 时返回 null —— 直接 `{flow.failureBanner}` 嵌进 JSX 即可。
   */
  failureBanner: ReactNode;
}

export function useRetryableFlow<T>({
  operation,
  invoke,
  onSuccess,
  retryable = true,
  onAbandon,
}: UseRetryableFlowOpts<T>): UseRetryableFlowReturn<T> {
  const { t } = useTranslation("common");
  const [state, setState] = useState<FlowState>("idle");
  const [result, setResult] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [attempts, setAttempts] = useState(0);

  // running 期间持有"当前一轮"的 abort 标志：用于丢弃旧 in-flight
  // promise 的回调（用户极快地连点重试时防错乱）。
  const generationRef = useRef(0);

  const run = useCallback(() => {
    if (state === "running") return;
    const myGen = ++generationRef.current;
    setState("running");
    setError(null);
    setAttempts((n) => n + 1);

    invoke()
      .then((r) => {
        if (myGen !== generationRef.current) return; // 已被新一轮覆盖
        setResult(r);
        setState("success");
        onSuccess?.(r);
      })
      .catch((e) => {
        if (myGen !== generationRef.current) return;
        const msg = formatBackendError(e);
        // 内部统一日志，方便排查"某流程多次失败"
        // eslint-disable-next-line no-console
        console.warn(`[retryable-flow] ${operation} failed:`, msg);
        setError(msg);
        setState("error");
      });
  }, [state, invoke, onSuccess, operation]);

  const reset = useCallback(() => {
    generationRef.current++; // 让任何 in-flight 回调被丢弃
    setState("idle");
    setError(null);
  }, []);

  const handleAbandon = useCallback(() => {
    reset();
    onAbandon?.();
  }, [reset, onAbandon]);

  const failureBanner = useMemo<ReactNode>(() => {
    if (state !== "error" || !error) return null;
    return (
      <div className={styles.banner} role="alert" aria-live="assertive">
        <div className={styles.iconCol}>!</div>
        <div className={styles.bodyCol}>
          <div className={styles.errorMessage}>{error}</div>
          {attempts > 1 && (
            <div className={styles.attemptsHint}>
              {t("flowRetry.attempts", { count: attempts })}
            </div>
          )}
          <div className={styles.actions}>
            {retryable && (
              <button
                type="button"
                className={`${styles.btn} ${styles.btnPrimary}`}
                onClick={run}
              >
                {t("flowRetry.retry")}
              </button>
            )}
            <button
              type="button"
              className={`${styles.btn} ${styles.btnGhost}`}
              onClick={handleAbandon}
            >
              {t("flowRetry.abandon")}
            </button>
          </div>
        </div>
      </div>
    );
  }, [state, error, attempts, retryable, run, handleAbandon, t]);

  return { state, result, error, attempts, run, reset, failureBanner };
}
