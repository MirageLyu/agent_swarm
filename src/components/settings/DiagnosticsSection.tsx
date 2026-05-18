/**
 * MVP polish: 让用户能一键导出最近日志 + 应用元信息，方便附在 issue 里。
 *
 * 调用 backend `export_diagnostics` 命令；用户通过 native save dialog 选路径。
 * 后端会做 API key 脱敏 + 用户名脱敏，前端不需要重复处理。
 */
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { save } from "@tauri-apps/plugin-dialog";
import { Button } from "../ui";
import { commands } from "../../ipc/commands";
import styles from "./DiagnosticsSection.module.css";

export function DiagnosticsSection() {
  const { t } = useTranslation("settings");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const handleExport = async () => {
    setMessage(null);
    setError(null);

    let target: string | null = null;
    try {
      target = await save({
        title: t("exportDiagnostics"),
        defaultPath: `miragenty-diagnostics-${new Date().toISOString().slice(0, 10)}.txt`,
        filters: [{ name: "Text", extensions: ["txt"] }],
      });
    } catch (e) {
      setError(t("exportDiagnosticsError", { message: e instanceof Error ? e.message : String(e) }));
      return;
    }
    if (!target) {
      return;
    }

    setBusy(true);
    try {
      const res = await commands.exportDiagnostics({
        output_path: target,
        log_tail_lines: 2000,
      });
      setMessage(
        t("exportDiagnosticsSuccess", {
          size: formatBytes(res.bytes_written),
          path: res.output_path,
          count: res.log_files_included,
        }),
      );
    } catch (e) {
      setError(t("exportDiagnosticsError", { message: e instanceof Error ? e.message : String(e) }));
    } finally {
      setBusy(false);
    }
  };

  const handleOpenLogs = async () => {
    setMessage(null);
    setError(null);
    try {
      const path = await commands.openLogDirectory();
      setMessage(
        t("openLogDirectorySuccess", { defaultValue: "Opened logs folder: {{path}}", path }),
      );
    } catch (e) {
      setError(
        t("openLogDirectoryError", {
          defaultValue: "Failed to open logs folder: {{message}}",
          message: e instanceof Error ? e.message : String(e),
        }),
      );
    }
  };

  return (
    <div className={styles.section}>
      <h3 className={styles.title}>{t("diagnosticsHeader")}</h3>
      <p className={styles.intro}>{t("diagnosticsIntro")}</p>
      <div className={styles.row}>
        <Button variant="secondary" onClick={handleExport} disabled={busy}>
          {busy ? t("exporting") : t("exportDiagnostics")}
        </Button>
        {/* 一键打开日志目录——用户报"agent 卡住"时让他们把最新 miragenty.log.* 文件
            发回来即可定位。比 Export 包更轻量，适合在线调试场景。 */}
        <Button variant="secondary" onClick={handleOpenLogs}>
          {t("openLogDirectory", { defaultValue: "Open log folder" })}
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
