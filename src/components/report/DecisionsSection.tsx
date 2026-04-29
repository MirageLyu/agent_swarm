import { useCallback, useMemo, useState } from "react";
import { commands, type DecisionVoteView, type MissionReportDecision } from "../../ipc/commands";
import { useReportStore } from "../../stores/report-store";
import styles from "./DecisionsSection.module.css";

interface Props {
  reportId: string;
  missionId: string;
  decisions: MissionReportDecision[];
  votes: DecisionVoteView[];
}

/**
 * FM-12 FR-04: Architecture Decisions 节
 *
 * 每张卡片：title / rationale / trade-off / risk + Agree/Disagree 投票
 * 投票走 commands.voteDecision；UNIQUE(report_id, decision_id) 让重复投票变成切换。
 */
export function DecisionsSection({ reportId, missionId, decisions, votes }: Props) {
  const recordVote = useReportStore((s) => s.recordVote);

  // votes 的索引 lookup
  const voteMap = useMemo(() => {
    const m = new Map<string, "agree" | "disagree">();
    votes.forEach((v) => m.set(v.decision_id, v.vote));
    return m;
  }, [votes]);

  return (
    <div className={styles.list}>
      {decisions.map((d) => (
        <DecisionCard
          key={d.id}
          reportId={reportId}
          missionId={missionId}
          decision={d}
          currentVote={voteMap.get(d.id)}
          onVoted={recordVote}
        />
      ))}
    </div>
  );
}

interface CardProps {
  reportId: string;
  missionId: string;
  decision: MissionReportDecision;
  currentVote: "agree" | "disagree" | undefined;
  onVoted: (missionId: string, decisionId: string, vote: "agree" | "disagree") => void;
}

function DecisionCard({ reportId, missionId, decision, currentVote, onVoted }: CardProps) {
  const [submitting, setSubmitting] = useState<"agree" | "disagree" | null>(null);
  const [error, setError] = useState<string | null>(null);

  const handleVote = useCallback(
    async (vote: "agree" | "disagree") => {
      setSubmitting(vote);
      setError(null);
      try {
        await commands.voteDecision({
          report_id: reportId,
          decision_id: decision.id,
          vote,
        });
        onVoted(missionId, decision.id, vote);
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        setSubmitting(null);
      }
    },
    [reportId, missionId, decision.id, onVoted],
  );

  return (
    <article className={styles.card}>
      <header className={styles.cardHeader}>
        <span className={styles.cardId}>{decision.id}</span>
        <h3 className={styles.cardTitle}>{decision.title}</h3>
      </header>

      <p className={styles.field}>
        <span className={styles.fieldLabel}>Rationale</span>
        <span className={styles.fieldValue}>{decision.rationale}</span>
      </p>
      {decision.trade_off && (
        <p className={styles.field}>
          <span className={styles.fieldLabel}>Trade-off</span>
          <span className={styles.fieldValue}>{decision.trade_off}</span>
        </p>
      )}
      {decision.risk && (
        <p className={styles.field}>
          <span className={styles.fieldLabel}>Risk</span>
          <span className={styles.fieldValue}>{decision.risk}</span>
        </p>
      )}

      <footer className={styles.voteRow}>
        <VoteButton
          tone="agree"
          active={currentVote === "agree"}
          submitting={submitting === "agree"}
          onClick={() => void handleVote("agree")}
        >
          Agree
        </VoteButton>
        <VoteButton
          tone="disagree"
          active={currentVote === "disagree"}
          submitting={submitting === "disagree"}
          onClick={() => void handleVote("disagree")}
        >
          Disagree
        </VoteButton>
        {error && <span className={styles.voteError}>{error}</span>}
      </footer>
    </article>
  );
}

function VoteButton(props: {
  tone: "agree" | "disagree";
  active: boolean;
  submitting: boolean;
  onClick: () => void;
  children: React.ReactNode;
}) {
  const { tone, active, submitting, onClick, children } = props;
  return (
    <button
      type="button"
      className={`${styles.voteButton} ${styles[`vote_${tone}`]} ${
        active ? styles.voteActive : ""
      }`}
      onClick={onClick}
      disabled={submitting}
    >
      {submitting ? "…" : children}
    </button>
  );
}
