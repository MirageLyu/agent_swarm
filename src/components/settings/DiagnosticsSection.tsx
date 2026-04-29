/**
 * MVP polish: 让用户能一键导出最近日志 + 应用元信息，方便附在 issue 里。
 *
 * 调用 backend `export_diagnostics` 命令；用户通过 native save dialog 选路径。
 * 后端会做 API key 脱敏 + 用户名脱敏，前端不需要重复处理。
 */
import { useState } from "react";
import { save } from "@tauri-apps/plugin-dialog";
import { Button } from "../ui";
import { commands } from "../../ipc/commands";
import styles from "./DiagnosticsSection.module.css";

export function DiagnosticsSection() {
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const handleExport = async () => {
    setMessage(null);
    setError(null);

    let target: string | null = null;
    try {
      target = await save({
        title: "Save diagnostic bundle",
        defaultPath: `miragenty-diagnostics-${new Date().toISOString().slice(0, 10)}.txt`,
        filters: [{ name: "Text", extensions: ["txt"] }],
      });
    } catch (e) {
      setError(`Dialog error: ${e instanceof Error ? e.message : String(e)}`);
      return;
    }
    if (!target) {
      // 用户取消，安静返回
      return;
    }

    setBusy(true);
    try {
      const res = await commands.exportDiagnostics({
        output_path: target,
        log_tail_lines: 2000,
      });
      setMessage(
        `Wrote ${formatBytes(res.bytes_written)} to ${res.output_path} ` +
          `(${res.log_files_included} log file(s) included).`,
      );
    } catch (e) {
      setError(`Export failed: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className={styles.section}>
      <h3 className={styles.title}>Diagnostics</h3>
      <p className={styles.intro}>
        Export a text bundle with the latest backend logs, app version, database
        summary, and recent errors. Use this when filing a bug report — sensitive
        data (API keys, your home username) is redacted automatically.
      </p>
      <div className={styles.row}>
        <Button variant="secondary" onClick={handleExport} disabled={busy}>
          {busy ? "Exporting..." : "Export Diagnostics"}
        </Button>
      </div>
      {message && <p className={styles.success}>{message}</p>}
      {error && <p className={styles.error}>{error}</p>}
    </div>
  );
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(2)} MB`;
}
