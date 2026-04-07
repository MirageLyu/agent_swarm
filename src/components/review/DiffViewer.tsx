import { DiffEditor } from "@monaco-editor/react";
import type { DiffFile } from "../../ipc";
import styles from "./DiffViewer.module.css";

interface DiffViewerProps {
  file: DiffFile | null;
  theme: "light" | "dark";
}

const extLanguageMap: Record<string, string> = {
  ts: "typescript",
  tsx: "typescript",
  js: "javascript",
  jsx: "javascript",
  rs: "rust",
  py: "python",
  json: "json",
  css: "css",
  html: "html",
  md: "markdown",
  toml: "toml",
  yaml: "yaml",
  yml: "yaml",
  sql: "sql",
  sh: "shell",
  bash: "shell",
  xml: "xml",
  svg: "xml",
};

function detectLanguage(path: string): string {
  const ext = path.split(".").pop()?.toLowerCase() ?? "";
  return extLanguageMap[ext] ?? "plaintext";
}

export function DiffViewer({ file, theme }: DiffViewerProps) {
  if (!file) {
    return <div className={styles.placeholder}>Select a file to view diff</div>;
  }

  const isBinary = file.old_content === null && file.new_content === null;
  if (isBinary) {
    return (
      <div className={styles.binaryNotice}>
        Binary file — cannot display diff
      </div>
    );
  }

  const language = detectLanguage(file.path);

  return (
    <div className={styles.container}>
      <div className={styles.fileHeader}>
        <span className={styles.filePath}>{file.path}</span>
      </div>
      <div className={styles.editorWrapper}>
        <DiffEditor
          original={file.old_content ?? ""}
          modified={file.new_content ?? ""}
          language={language}
          theme={theme === "dark" ? "vs-dark" : "vs"}
          options={{
            readOnly: true,
            renderSideBySide: true,
            minimap: { enabled: false },
            scrollBeyondLastLine: false,
            fontSize: 13,
            fontFamily: "var(--font-mono)",
            lineNumbers: "on",
            wordWrap: "off",
            automaticLayout: true,
          }}
        />
      </div>
    </div>
  );
}
