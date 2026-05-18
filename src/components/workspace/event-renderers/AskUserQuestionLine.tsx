import { memo, useEffect, useMemo, useState } from "react";
import type { AgentEvent } from "../../../stores/agent-store";
import { useAgentStore } from "../../../stores/agent-store";
import { commands } from "../../../ipc/commands";
import styles from "./EventLine.module.css";
import askStyles from "./AskUserQuestionLine.module.css";

interface QuestionOption {
  id: string;
  label: string;
}

interface QuestionShape {
  id: string;
  prompt: string;
  options: QuestionOption[];
  allow_multiple?: boolean;
}

interface AskMeta {
  kind: "ask_user_question";
  session_id: string;
  agent_id?: string;
  questions: QuestionShape[];
}

function isAskMeta(value: unknown): value is AskMeta {
  if (!value || typeof value !== "object") return false;
  const v = value as { kind?: unknown; session_id?: unknown; questions?: unknown };
  return (
    v.kind === "ask_user_question" &&
    typeof v.session_id === "string" &&
    Array.isArray(v.questions)
  );
}

interface AskUserQuestionLineProps {
  event: AgentEvent;
}

/**
 * Single-Agent Uplift B1: 渲染 LLM 调 ask_user_question 后的问题卡片。
 *
 * 状态由 agent-store 的 `resolvedQuestionSessions` 管理 —— 当后端 emit
 * `ask_user_question_resolved` 事件时，store 把 session_id 标记为 resolved，
 * 我们这里据此把卡片切换到"已回答"显示态，避免再让用户重复提交。
 */
export const AskUserQuestionLine = memo(function AskUserQuestionLine({
  event,
}: AskUserQuestionLineProps) {
  const meta = isAskMeta(event.meta) ? event.meta : null;
  const resolvedSessions = useAgentStore((s) => s.resolvedQuestionSessions);
  const isResolved = meta ? resolvedSessions.has(meta.session_id) : true;

  // 每个 question 的当前选择：question_id → Set(option_id)
  const [picked, setPicked] = useState<Record<string, Set<string>>>({});
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (meta) {
      const init: Record<string, Set<string>> = {};
      for (const q of meta.questions) init[q.id] = new Set();
      setPicked(init);
    }
  }, [meta?.session_id]);

  const allAnswered = useMemo(() => {
    if (!meta) return false;
    return meta.questions.every((q) => (picked[q.id]?.size ?? 0) > 0);
  }, [meta, picked]);

  if (!meta) {
    // meta 不合法 → 退化到原 SystemHint 视觉
    return (
      <div className={styles.systemHint}>
        <span className={styles.systemHintLabel}>System Hint</span>
        <span>{event.content}</span>
      </div>
    );
  }

  const togglePick = (q: QuestionShape, optionId: string) => {
    setPicked((prev) => {
      const next = { ...prev };
      const current = new Set(next[q.id] ?? []);
      if (q.allow_multiple) {
        if (current.has(optionId)) current.delete(optionId);
        else current.add(optionId);
      } else {
        current.clear();
        current.add(optionId);
      }
      next[q.id] = current;
      return next;
    });
  };

  const handleSubmit = async () => {
    if (!allAnswered || submitting) return;
    setSubmitting(true);
    setError(null);
    try {
      const payload: Record<string, string[]> = {};
      for (const q of meta.questions) {
        payload[q.id] = Array.from(picked[q.id] ?? []);
      }
      await commands.submitUserQuestionAnswer(meta.session_id, payload);
      // 不在这里手动 setResolved —— 等后端 ask_user_question_resolved 事件回流统一处理，
      // 避免乐观更新和真实状态出现裂缝。
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div className={askStyles.card}>
      <div className={askStyles.header}>
        <span className={askStyles.icon}>❓</span>
        <span className={askStyles.title}>
          Agent is asking {meta.questions.length === 1 ? "a question" : `${meta.questions.length} questions`}
        </span>
        {isResolved && <span className={askStyles.resolvedBadge}>resolved</span>}
      </div>
      {meta.questions.map((q) => (
        <div key={q.id} className={askStyles.question}>
          <div className={askStyles.prompt}>{q.prompt}</div>
          <div className={askStyles.options}>
            {q.options.map((opt) => {
              const selected = picked[q.id]?.has(opt.id) ?? false;
              return (
                <button
                  key={opt.id}
                  type="button"
                  className={`${askStyles.option} ${selected ? askStyles.optionSelected : ""}`}
                  disabled={isResolved || submitting}
                  onClick={() => togglePick(q, opt.id)}
                >
                  <span className={askStyles.optionMarker}>
                    {q.allow_multiple
                      ? selected
                        ? "☑"
                        : "☐"
                      : selected
                        ? "●"
                        : "○"}
                  </span>
                  {opt.label}
                </button>
              );
            })}
          </div>
          {q.allow_multiple && (
            <div className={askStyles.hint}>multiple selections allowed</div>
          )}
        </div>
      ))}
      {error && <div className={askStyles.error}>{error}</div>}
      <div className={askStyles.footer}>
        {!isResolved && (
          <button
            type="button"
            className={askStyles.submit}
            disabled={!allAnswered || submitting}
            onClick={handleSubmit}
          >
            {submitting ? "Submitting…" : "Submit answer"}
          </button>
        )}
        {isResolved && (
          <span className={askStyles.resolvedHint}>
            Answer submitted — agent has resumed.
          </span>
        )}
      </div>
    </div>
  );
});
