import { useState, useCallback, useEffect, useRef } from "react";
import { Button } from "../ui";
import { PlannerStreamPanel, type PlannerStreamState } from "./PlannerStreamPanel";
import { onPlannerStream, type PlannerStreamPayload } from "../../ipc/events";
import styles from "./PlanInput.module.css";

interface PlanInputProps {
  onPlan: (description: string) => void;
  onCancel?: () => void;
  loading: boolean;
}

const MAX_CHARS = 2000;

export function PlanInput({ onPlan, onCancel, loading }: PlanInputProps) {
  const [text, setText] = useState("");
  const [stream, setStream] = useState<PlannerStreamState>({
    visible: false,
    text: "",
    tokenCount: 0,
    elapsedMs: 0,
    status: "streaming",
    collapsed: false,
  });

  const timerRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const startTimeRef = useRef<number>(0);
  const cancelledRef = useRef(false);

  const canSubmit = text.trim().length > 0 && !loading;

  // Start timer when loading begins, stop when loading ends
  useEffect(() => {
    if (loading) {
      cancelledRef.current = false;
      startTimeRef.current = Date.now();
      setStream({
        visible: true,
        text: "",
        tokenCount: 0,
        elapsedMs: 0,
        status: "streaming",
        collapsed: false,
      });
      timerRef.current = setInterval(() => {
        setStream((s) => ({
          ...s,
          elapsedMs: Date.now() - startTimeRef.current,
        }));
      }, 200);
    } else {
      if (timerRef.current) {
        clearInterval(timerRef.current);
        timerRef.current = null;
      }
    }
    return () => {
      if (timerRef.current) clearInterval(timerRef.current);
    };
  }, [loading]);

  // Subscribe to planner stream events
  useEffect(() => {
    const unsub = onPlannerStream((payload: PlannerStreamPayload) => {
      if (cancelledRef.current) return;

      if (payload.kind === "reasoning_delta" || payload.kind === "text_delta") {
        setStream((s) => ({
          ...s,
          text: s.text + payload.content,
          tokenCount: s.tokenCount + 1,
        }));
      } else if (payload.kind === "done") {
        setStream((s) => ({
          ...s,
          status: "done",
          collapsed: true,
          elapsedMs: Date.now() - startTimeRef.current,
        }));
      } else if (payload.kind === "error") {
        setStream((s) => ({
          ...s,
          status: "error",
          errorMessage: payload.content,
          elapsedMs: Date.now() - startTimeRef.current,
        }));
      }
    });

    return () => {
      unsub.then((fn) => fn());
    };
  }, []);

  const handleSubmit = useCallback(() => {
    if (!canSubmit) return;
    onPlan(text.trim());
  }, [text, canSubmit, onPlan]);

  const handleCancel = useCallback(() => {
    cancelledRef.current = true;
    setStream((s) => ({
      ...s,
      status: "cancelled",
      elapsedMs: Date.now() - startTimeRef.current,
    }));
    onCancel?.();
  }, [onCancel]);

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
      e.preventDefault();
      handleSubmit();
    }
  };

  const toggleCollapse = useCallback(() => {
    setStream((s) => ({ ...s, collapsed: !s.collapsed }));
  }, []);

  return (
    <div className={styles.container}>
      <textarea
        className={styles.textarea}
        value={text}
        onChange={(e) => setText(e.target.value.slice(0, MAX_CHARS))}
        onKeyDown={handleKeyDown}
        placeholder="Describe your mission... (e.g., Build a user authentication system with login, registration, and password reset)"
        disabled={loading}
        rows={3}
      />
      <div className={styles.footer}>
        <span className={styles.charCount}>
          {text.length}/{MAX_CHARS}
        </span>
        <div className={styles.actions}>
          <span className={styles.hint}>
            <kbd className={styles.kbd}>{navigator.platform?.includes("Mac") ? "\u2318" : "Ctrl"}</kbd>
            <kbd className={styles.kbd}>Enter</kbd>
          </span>
          {loading ? (
            <Button variant="secondary" size="sm" onClick={handleCancel}>
              Cancel
            </Button>
          ) : null}
          <Button
            variant="primary"
            size="sm"
            onClick={handleSubmit}
            disabled={!canSubmit}
          >
            {loading ? "Planning\u2026" : "Plan Mission"}
          </Button>
        </div>
      </div>
      <PlannerStreamPanel state={stream} onToggleCollapse={toggleCollapse} />
    </div>
  );
}
