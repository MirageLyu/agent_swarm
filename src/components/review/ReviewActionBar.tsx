import { useState } from "react";
import { Button } from "../ui";
import type { ReviewAction } from "../../ipc";
import styles from "./ReviewActionBar.module.css";

interface ReviewActionBarProps {
  currentStatus: ReviewAction | null;
  disabled: boolean;
  onAction: (action: ReviewAction, comment?: string) => void;
}

export function ReviewActionBar({ currentStatus, disabled, onAction }: ReviewActionBarProps) {
  const [showRevisionDialog, setShowRevisionDialog] = useState(false);
  const [comment, setComment] = useState("");

  const handleRevisionSubmit = () => {
    if (comment.trim()) {
      onAction("revision_requested", comment.trim());
      setShowRevisionDialog(false);
      setComment("");
    }
  };

  return (
    <>
      <div className={styles.bar}>
        {currentStatus && (
          <span className={styles.statusText}>
            Status: {currentStatus === "approved" ? "Approved" : currentStatus === "rejected" ? "Rejected" : "Revision Requested"}
          </span>
        )}

        <Button
          variant="ghost"
          size="sm"
          disabled={disabled}
          onClick={() => onAction("rejected")}
        >
          Reject
        </Button>

        <Button
          variant="secondary"
          size="sm"
          disabled={disabled}
          onClick={() => setShowRevisionDialog(true)}
        >
          Request Revision
        </Button>

        <Button
          variant="primary"
          size="sm"
          disabled={disabled}
          onClick={() => onAction("approved")}
        >
          Approve
        </Button>
      </div>

      {showRevisionDialog && (
        <div className={styles.dialogOverlay} onClick={() => setShowRevisionDialog(false)}>
          <div className={styles.dialogContent} onClick={(e) => e.stopPropagation()}>
            <div className={styles.dialogTitle}>Request Revision</div>
            <textarea
              className={styles.dialogTextarea}
              placeholder="Describe what changes are needed..."
              value={comment}
              onChange={(e) => setComment(e.target.value)}
              autoFocus
            />
            <div className={styles.dialogActions}>
              <Button variant="ghost" size="sm" onClick={() => setShowRevisionDialog(false)}>
                Cancel
              </Button>
              <Button
                variant="primary"
                size="sm"
                disabled={!comment.trim()}
                onClick={handleRevisionSubmit}
              >
                Submit
              </Button>
            </div>
          </div>
        </div>
      )}
    </>
  );
}
