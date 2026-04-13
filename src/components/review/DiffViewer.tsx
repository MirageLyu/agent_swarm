import { DiffEditor } from "@monaco-editor/react";
import type { DiffFile, AnnotationInfo } from "../../ipc";
import { FileScore } from "./FileScore";
import { EvaluatorAnnotation } from "./EvaluatorAnnotation";
import styles from "./DiffViewer.module.css";

interface DiffViewerProps {
  file: DiffFile | null;
  theme: "light" | "dark";
  annotations?: AnnotationInfo[];
  fileScore?: number | null;
  onAnnotationStatusChange?: (id: string, newStatus: string) => void;
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

export function DiffViewer({ file, theme, annotations, fileScore, onAnnotationStatusChange }: DiffViewerProps) {
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
  const visibleAnnotations = annotations?.filter((a) => a.status !== "dismissed") ?? [];

  return (
    <div className={styles.container}>
      <div className={styles.fileHeader}>
        <span className={styles.filePath}>{file.path}</span>
        {fileScore != null && <FileScore score={fileScore} />}
      </div>

      {visibleAnnotations.length > 0 && (
        <div className={styles.annotationsPanel}>
          {visibleAnnotations.map((ann) => (
            <EvaluatorAnnotation
              key={ann.id}
              annotation={ann}
              onStatusChange={onAnnotationStatusChange}
            />
          ))}
        </div>
      )}

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
