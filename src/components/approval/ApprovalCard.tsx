/**
 * FM-14: 单条审批卡片。
 *
 * 按 `kind` 渲染不同 payload 摘要 + 不同操作按钮：
 * - `tool`        : Approve / Reject
 * - `fetch`       : Allow Once / Allow Session / Reject
 * - `escalation`  : Approve / Reject（approve 走旧 confirm_followup_proposal，
 *                   见下面 invoker 备注；reject 走 reject_followup_proposal）
 * - `budget`      : Approve / Reject
 * - `chat_commit` : Approve / Reject
 *
 * 出于复用考虑：approve/reject 都通过 `commands.resolveApproval` 走统一通道；
 * 旧 IPC（confirm_planner_fetch / confirm_followup_proposal）仍可用，但 UI 走新通道
 * 比较干净，避免双写状态。
 */
import { useEffect, useMemo, useState } from "react";
import { Badge, Button } from "../ui";
import { commands, type ApprovalView } from "../../ipc/commands";
import { useApprovalStore } from "../../stores/approval-store";
import { RejectDialog } from "./RejectDialog";
import styles from "./ApprovalCard.module.css";

interface ApprovalCardProps {
  approval: ApprovalView;
}

export function ApprovalCard({ approval }: ApprovalCardProps) {
  const removeLocal = useApprovalStore((s) => s.removeLocal);
  const [busy, setBusy] = useState(false);
  const [rejectOpen, setRejectOpen] = useState(false);
  const [now, setNow] = useState(() => Date.now());

  // 每秒刷新一下倒计时；卡片量小（pending 一般 < 10），不会成为性能问题。
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, []);

  const remaining = useMemo(() => {
    const exp = Date.parse(approval.expires_at + "Z");
    if (Number.isNaN(exp)) return null;
    const diff = Math.max(0, Math.floor((exp - now) / 1000));
    if (diff <= 0) return "expiring…";
    if (diff < 60) return `${diff}s left`;
    if (diff < 3600) return `${Math.floor(diff / 60)}m left`;
    return `${Math.floor(diff / 3600)}h left`;
  }, [approval.expires_at, now]);

  const payload = useMemo(() => {
    try {
      return JSON.parse(approval.payload || "{}") as Record<string, unknown>;
    } catch {
      return {} as Record<string, unknown>;
    }
  }, [approval.payload]);

  const resolve = async (decision: "approved" | "rejected", note?: string) => {
    setBusy(true);
    try {
      await commands.resolveApproval({
        request_id: approval.id,
        decision,
        note: note ?? null,
      });
      // 乐观从本地列表移除；后端 emit 'approval-resolved' 会再保证移除一次（幂等）。
      removeLocal(approval.id);
    } catch (e) {
      console.warn("[approval] resolve failed", e);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className={styles.card} data-kind={approval.kind}>
      <div className={styles.header}>
        <div className={styles.titleBlock}>
          <div className={styles.kindRow}>
            <KindBadge kind={approval.kind} />
            <span className={styles.title}>{approval.title}</span>
          </div>
        </div>
        {remaining && <span className={styles.expires}>{remaining}</span>}
      </div>

      <KindMeta kind={approval.kind} payload={payload} />

      {approval.reason.trim() && (
        <p className={styles.reason}>"{approval.reason}"</p>
      )}

      <div className={styles.actions}>
        {renderActions(approval.kind, busy, resolve, () => setRejectOpen(true))}
      </div>

      <RejectDialog
        open={rejectOpen}
        approvalTitle={approval.title}
        onCancel={() => setRejectOpen(false)}
        onConfirm={async (note) => {
          setRejectOpen(false);
          await resolve("rejected", note || undefined);
        }}
      />
    </div>
  );
}

function KindBadge({ kind }: { kind: ApprovalView["kind"] }) {
  const variant = (() => {
    switch (kind) {
      case "tool":
        return "warning" as const;
      case "fetch":
        return "info" as const;
      case "escalation":
        return "info" as const;
      case "budget":
        return "error" as const;
      case "chat_commit":
        return "warning" as const;
      default:
        return "default" as const;
    }
  })();
  return <Badge variant={variant}>{kindLabel(kind)}</Badge>;
}

function kindLabel(kind: ApprovalView["kind"]): string {
  switch (kind) {
    case "tool":
      return "Tool";
    case "fetch":
      return "Fetch";
    case "escalation":
      return "Escalate";
    case "budget":
      return "Budget";
    case "chat_commit":
      return "Chat Commit";
    default:
      return kind;
  }
}

function KindMeta({
  kind,
  payload,
}: {
  kind: ApprovalView["kind"];
  payload: Record<string, unknown>;
}) {
  if (kind === "fetch") {
    return (
      <dl className={styles.meta}>
        <div className={styles.metaRow}>
          <dt>URL</dt>
          <dd>
            <code className={styles.code}>{String(payload.url ?? "")}</code>
          </dd>
        </div>
        <div className={styles.metaRow}>
          <dt>Host</dt>
          <dd>
            <code className={styles.code}>{String(payload.host ?? "")}</code>
          </dd>
        </div>
      </dl>
    );
  }
  if (kind === "tool") {
    const toolName = String(payload.tool_name ?? payload.tool ?? "");
    const summary = String(payload.summary ?? payload.input_preview ?? "");
    return (
      <dl className={styles.meta}>
        {toolName && (
          <div className={styles.metaRow}>
            <dt>Tool</dt>
            <dd>
              <code className={styles.code}>{toolName}</code>
            </dd>
          </div>
        )}
        {summary && (
          <div className={styles.metaRow}>
            <dt>Args</dt>
            <dd>{summary}</dd>
          </div>
        )}
      </dl>
    );
  }
  if (kind === "escalation") {
    const tasks = payload.estimated_tasks ?? payload.estimatedTasks;
    const requestSummary = payload.request_summary;
    return (
      <dl className={styles.meta}>
        {tasks != null && (
          <div className={styles.metaRow}>
            <dt>Tasks</dt>
            <dd>~{String(tasks)}</dd>
          </div>
        )}
        {requestSummary != null && String(requestSummary).trim() !== "" && (
          <div className={styles.metaRow}>
            <dt>Request</dt>
            <dd>{String(requestSummary)}</dd>
          </div>
        )}
      </dl>
    );
  }
  if (kind === "budget") {
    return (
      <dl className={styles.meta}>
        <div className={styles.metaRow}>
          <dt>Used</dt>
          <dd>${String(payload.used_usd ?? "?")}</dd>
        </div>
        <div className={styles.metaRow}>
          <dt>Budget</dt>
          <dd>${String(payload.budget_usd ?? "?")}</dd>
        </div>
      </dl>
    );
  }
  if (kind === "chat_commit") {
    return (
      <dl className={styles.meta}>
        <div className={styles.metaRow}>
          <dt>Files</dt>
          <dd>{String(payload.files_changed ?? "?")}</dd>
        </div>
        <div className={styles.metaRow}>
          <dt>Lines</dt>
          <dd>{String(payload.lines_changed ?? "?")}</dd>
        </div>
      </dl>
    );
  }
  return null;
}

function renderActions(
  kind: ApprovalView["kind"],
  busy: boolean,
  resolve: (d: "approved" | "rejected", note?: string) => Promise<void>,
  openReject: () => void,
) {
  if (kind === "fetch") {
    return (
      <>
        <Button variant="secondary" size="sm" onClick={openReject} disabled={busy}>
          Reject
        </Button>
        <Button
          variant="secondary"
          size="sm"
          onClick={() => resolve("approved", "session")}
          disabled={busy}
        >
          Allow this session
        </Button>
        <Button
          variant="primary"
          size="sm"
          onClick={() => resolve("approved", "once")}
          disabled={busy}
        >
          Allow once
        </Button>
      </>
    );
  }
  return (
    <>
      <Button variant="secondary" size="sm" onClick={openReject} disabled={busy}>
        Reject
      </Button>
      <Button
        variant="primary"
        size="sm"
        onClick={() => resolve("approved")}
        disabled={busy}
      >
        Approve
      </Button>
    </>
  );
}
