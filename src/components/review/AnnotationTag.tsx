import type { AnnotationType, AnnotationSeverity, AnnotationStatus } from "../../ipc";
import styles from "./AnnotationTag.module.css";

interface AnnotationTagProps {
  type?: AnnotationType;
  severity?: AnnotationSeverity;
  status?: AnnotationStatus;
}

const typeLabels: Record<AnnotationType, string> = {
  bug: "Bug",
  style: "Style",
  performance: "Perf",
  security: "Security",
  suggestion: "Suggestion",
};

const statusLabels: Record<AnnotationStatus, string> = {
  open: "Needs Review",
  auto_fixed: "Auto-fixed",
  revision_requested: "Requested",
  dismissed: "Dismissed",
};

export function AnnotationTag({ type, severity, status }: AnnotationTagProps) {
  if (status) {
    return (
      <span className={`${styles.tag} ${styles[`status_${status}`]}`}>
        {statusLabels[status]}
      </span>
    );
  }

  if (type) {
    const severityClass = severity ? styles[severity] : "";
    return (
      <span className={`${styles.tag} ${styles.typeTag} ${severityClass}`}>
        {typeLabels[type]}
      </span>
    );
  }

  return null;
}
