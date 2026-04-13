import { useState, useCallback } from "react";
import type { AnnotationInfo } from "../../ipc";
import { commands } from "../../ipc";
import { AnnotationTag } from "./AnnotationTag";
import styles from "./EvaluatorAnnotation.module.css";

interface EvaluatorAnnotationProps {
  annotation: AnnotationInfo;
  onStatusChange?: (id: string, newStatus: string) => void;
}

export function EvaluatorAnnotation({ annotation, onStatusChange }: EvaluatorAnnotationProps) {
  const [status, setStatus] = useState(annotation.status);
  const [showOriginal, setShowOriginal] = useState(false);
  const [dismissing, setDismissing] = useState(false);

  const severityClass =
    annotation.severity === "error"
      ? styles.error
      : annotation.severity === "warning"
        ? styles.warning
        : styles.info;

  const handleDismiss = useCallback(async () => {
    setDismissing(true);
    try {
      await commands.updateAnnotationStatus({
        annotation_id: annotation.id,
        status: "dismissed",
      });
      setStatus("dismissed");
      onStatusChange?.(annotation.id, "dismissed");
    } catch {
      setDismissing(false);
    }
  }, [annotation.id, onStatusChange]);

  const handleRequestRevision = useCallback(async () => {
    try {
      await commands.updateAnnotationStatus({
        annotation_id: annotation.id,
        status: "revision_requested",
      });
      setStatus("revision_requested");
      onStatusChange?.(annotation.id, "revision_requested");
    } catch {}
  }, [annotation.id, onStatusChange]);

  if (status === "dismissed" && dismissing) {
    return <div className={`${styles.container} ${severityClass} ${styles.fadeOut}`} />;
  }

  if (status === "dismissed") {
    return null;
  }

  return (
    <div className={`${styles.container} ${severityClass}`}>
      <div className={styles.header}>
        <div className={styles.tags}>
          <AnnotationTag type={annotation.type} severity={annotation.severity} />
          {status === "auto_fixed" && <AnnotationTag status="auto_fixed" />}
          {status === "open" && !annotation.auto_fixable && <AnnotationTag status="open" />}
          {status === "revision_requested" && <AnnotationTag status="revision_requested" />}
        </div>
        <span className={styles.line}>L{annotation.line_number}</span>
      </div>

      <p className={styles.message}>{annotation.message}</p>

      {annotation.suggestion && (
        <p className={styles.suggestion}>{annotation.suggestion}</p>
      )}

      {status === "auto_fixed" && annotation.fixed_code && (
        <div className={styles.fixInfo}>
          <button
            className={styles.viewOriginal}
            onClick={() => setShowOriginal(!showOriginal)}
          >
            {showOriginal ? "Hide Original" : "View Original"}
          </button>
          {showOriginal && annotation.original_code && (
            <pre className={styles.codeBlock}>{annotation.original_code}</pre>
          )}
        </div>
      )}

      {status === "open" && (
        <div className={styles.actions}>
          <button className={styles.revisionBtn} onClick={handleRequestRevision}>
            Request Revision
          </button>
          <button className={styles.dismissBtn} onClick={handleDismiss}>
            Dismiss
          </button>
        </div>
      )}
    </div>
  );
}
