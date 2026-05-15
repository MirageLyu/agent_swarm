import { memo, useMemo } from "react";
import type { AgentEvent } from "../../../stores/agent-store";
import styles from "./EventLine.module.css";

interface NoteAppliedMeta {
  applied_count?: number;
  notes?: string[];
}

function asNoteMeta(meta: unknown): NoteAppliedMeta | null {
  if (typeof meta !== "object" || meta === null) return null;
  return meta as NoteAppliedMeta;
}

interface NoteAppliedLineProps {
  event: AgentEvent;
}

/// FM-06 Mid-flight note 注入。原本只展示 "Applied N note(s)"，看不到内容，
/// 这里把 directive 文本展开，最多 3 条；剩下的"+ N more"提示。
export const NoteAppliedLine = memo(function NoteAppliedLine({
  event,
}: NoteAppliedLineProps) {
  const meta = asNoteMeta(event.meta);
  const notes = useMemo(() => meta?.notes ?? [], [meta]);
  const visible = notes.slice(0, 3);
  const more = notes.length - visible.length;

  return (
    <div className={styles.note}>
      <div className={styles.noteHeader}>
        Commander Note · {meta?.applied_count ?? notes.length}
      </div>
      {visible.length === 0 ? (
        <div className={styles.noteBody}>{event.content}</div>
      ) : (
        <>
          {visible.map((n, i) => (
            <div key={i} className={styles.noteBody}>
              {visible.length > 1 ? `${i + 1}. ${n}` : n}
            </div>
          ))}
          {more > 0 && (
            <div className={styles.noteBody}>
              <em>+ {more} more</em>
            </div>
          )}
        </>
      )}
    </div>
  );
});
