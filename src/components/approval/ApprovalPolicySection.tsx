/**
 * FM-14: ApprovalPolicy 编辑器，挂在 Settings 页面里。
 *
 * 字段语义见 backend `ApprovalPolicy::default()`。
 * 列表型字段（protected_paths / destructive_commands）走"每行一个条目"的 textarea，
 * 保存时按换行拆分；这样比写 chip-input 简单且对用户更直观。
 */
import { useEffect, useState } from "react";
import { Button } from "../ui";
import { commands, type ApprovalPolicy } from "../../ipc/commands";
import styles from "./ApprovalPolicySection.module.css";

export function ApprovalPolicySection() {
  const [policy, setPolicy] = useState<ApprovalPolicy | null>(null);
  const [timeoutSec, setTimeoutSec] = useState("");
  const [protectedPaths, setProtectedPaths] = useState("");
  const [destructiveCmds, setDestructiveCmds] = useState("");
  const [budgetWarn, setBudgetWarn] = useState("");
  const [chatSoft, setChatSoft] = useState("");
  const [dirty, setDirty] = useState(false);
  const [saving, setSaving] = useState(false);
  const [message, setMessage] = useState("");

  const apply = (p: ApprovalPolicy) => {
    setPolicy(p);
    setTimeoutSec(String(p.timeout_seconds));
    setProtectedPaths(p.protected_paths.join("\n"));
    setDestructiveCmds(p.destructive_commands.join("\n"));
    setBudgetWarn(String(p.budget_warn_ratio));
    setChatSoft(String(p.chat_commit_soft_lines));
    setDirty(false);
  };

  useEffect(() => {
    commands
      .getApprovalPolicy()
      .then(apply)
      .catch((e) => setMessage(`Load failed: ${e}`));
  }, []);

  const save = async () => {
    setSaving(true);
    try {
      const next = await commands.updateApprovalPolicy({
        timeout_seconds: parseInt(timeoutSec, 10) || policy?.timeout_seconds || 600,
        protected_paths: splitLines(protectedPaths),
        destructive_commands: splitLines(destructiveCmds),
        budget_warn_ratio: clampFloat(budgetWarn, 0, 1),
        chat_commit_soft_lines:
          parseInt(chatSoft, 10) || policy?.chat_commit_soft_lines || 0,
      });
      apply(next);
      setMessage("Approval policy saved.");
      setTimeout(() => setMessage(""), 2500);
    } catch (e) {
      setMessage(`Save failed: ${e}`);
    } finally {
      setSaving(false);
    }
  };

  const markDirty = () => setDirty(true);

  return (
    <div className={styles.section}>
      <h2 className={styles.sectionTitle}>Approval Policy</h2>
      <p className={styles.intro}>
        Decide which agent actions need your sign-off before they run.
        Anything matched here is paused, mirrored to the approvals drawer in the
        top bar, and resumed only after you approve or reject it.
      </p>

      <div className={styles.field}>
        <label className={styles.label}>Approval timeout (seconds)</label>
        <input
          className={styles.input}
          type="number"
          value={timeoutSec}
          onChange={(e) => {
            setTimeoutSec(e.target.value);
            markDirty();
          }}
          min={30}
          max={3600}
        />
        <p className={styles.hint}>
          Pending requests auto-expire (treated as rejected) after this many seconds.
          Default 600s = 10min, intentionally below the agent wall-clock so a
          single approval never starves the run.
        </p>
      </div>

      <div className={styles.field}>
        <label className={styles.label}>Protected paths (one per line)</label>
        <textarea
          className={styles.textarea}
          rows={6}
          value={protectedPaths}
          onChange={(e) => {
            setProtectedPaths(e.target.value);
            markDirty();
          }}
          placeholder={"package.json\nsrc-tauri/tauri.conf.json\n.github/"}
        />
        <p className={styles.hint}>
          Workspace-relative path prefixes (POSIX style). Any write_file /
          delete_file targeting these paths will require approval.
        </p>
      </div>

      <div className={styles.field}>
        <label className={styles.label}>Destructive commands (one per line)</label>
        <textarea
          className={styles.textarea}
          rows={5}
          value={destructiveCmds}
          onChange={(e) => {
            setDestructiveCmds(e.target.value);
            markDirty();
          }}
          placeholder={"rm\ngit push\ngit reset"}
        />
        <p className={styles.hint}>
          Match against the first word(s) of any shell_exec call. Comparison is
          lower-cased and prefix-based.
        </p>
      </div>

      <div className={styles.field}>
        <label className={styles.label}>Budget warn ratio</label>
        <input
          className={styles.input}
          type="number"
          step="0.05"
          min={0}
          max={1}
          value={budgetWarn}
          onChange={(e) => {
            setBudgetWarn(e.target.value);
            markDirty();
          }}
        />
        <p className={styles.hint}>
          When mission cost reaches this fraction of the contract budget,
          a budget approval is queued. 0 = disable.
        </p>
      </div>

      <div className={styles.field}>
        <label className={styles.label}>Chat commit soft threshold (lines)</label>
        <input
          className={styles.input}
          type="number"
          min={0}
          value={chatSoft}
          onChange={(e) => {
            setChatSoft(e.target.value);
            markDirty();
          }}
        />
        <p className={styles.hint}>
          Chat agent commits over this many diff lines (but still under the hard
          30-line ceiling) require approval. 0 = no soft gate.
        </p>
      </div>

      {dirty && (
        <div className={styles.saveRow}>
          <Button variant="primary" onClick={save} disabled={saving}>
            {saving ? "Saving…" : "Save approval policy"}
          </Button>
        </div>
      )}
      {message && <p className={styles.message}>{message}</p>}
    </div>
  );
}

function splitLines(s: string): string[] {
  return s
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line.length > 0);
}

function clampFloat(s: string, min: number, max: number): number {
  const v = parseFloat(s);
  if (Number.isNaN(v)) return 0;
  return Math.min(max, Math.max(min, v));
}
