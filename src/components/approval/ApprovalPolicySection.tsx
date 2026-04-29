/**
 * FM-14: ApprovalPolicy 编辑器，挂在 Settings 页面里。
 *
 * 字段语义见 backend `ApprovalPolicy::default()`。
 * 列表型字段（protected_paths / destructive_commands）走"每行一个条目"的 textarea，
 * 保存时按换行拆分；这样比写 chip-input 简单且对用户更直观。
 */
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "../ui";
import { commands, type ApprovalPolicy } from "../../ipc/commands";
import styles from "./ApprovalPolicySection.module.css";

export function ApprovalPolicySection() {
  const { t } = useTranslation("settings");
  const { t: tc } = useTranslation("common");
  const { t: ta } = useTranslation("approval");
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
      setMessage(ta("policySaved"));
      setTimeout(() => setMessage(""), 2500);
    } catch (e) {
      setMessage(tc("errorPrefix", { message: String(e) }));
    } finally {
      setSaving(false);
    }
  };

  const markDirty = () => setDirty(true);

  return (
    <div className={styles.section}>
      <h2 className={styles.sectionTitle}>{t("approvalPolicyHeader")}</h2>
      <p className={styles.intro}>{t("approvalPolicyIntro")}</p>

      <div className={styles.field}>
        <label className={styles.label}>{t("approvalTimeoutLabel")}</label>
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
        <p className={styles.hint}>{t("approvalTimeoutHint")}</p>
      </div>

      <div className={styles.field}>
        <label className={styles.label}>{t("protectedPathsLabel")}</label>
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
        <p className={styles.hint}>{t("protectedPathsHint")}</p>
      </div>

      <div className={styles.field}>
        <label className={styles.label}>{t("destructiveCommandsLabel")}</label>
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
        <p className={styles.hint}>{t("destructiveCommandsHint")}</p>
      </div>

      <div className={styles.field}>
        <label className={styles.label}>{t("budgetWarnRatioLabel")}</label>
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
        <p className={styles.hint}>{t("budgetWarnRatioHint")}</p>
      </div>

      <div className={styles.field}>
        <label className={styles.label}>{t("chatCommitSoftLinesLabel")}</label>
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
        <p className={styles.hint}>{t("chatCommitSoftLinesHint")}</p>
      </div>

      {dirty && (
        <div className={styles.saveRow}>
          <Button variant="primary" onClick={save} disabled={saving}>
            {saving ? tc("saving") : tc("save")}
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
