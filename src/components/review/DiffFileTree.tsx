import type { DiffFile } from "../../ipc";
import styles from "./DiffFileTree.module.css";

interface DiffFileTreeProps {
  files: DiffFile[];
  selectedPath: string | null;
  onSelect: (path: string) => void;
}

const statusChar: Record<string, string> = {
  added: "A",
  modified: "M",
  deleted: "D",
};

export function DiffFileTree({ files, selectedPath, onSelect }: DiffFileTreeProps) {
  return (
    <div className={styles.container}>
      <div className={styles.header}>
        Changed Files ({files.length})
      </div>
      <div className={styles.list}>
        {files.length === 0 ? (
          <div className={styles.empty}>No changes</div>
        ) : (
          files.map((file) => (
            <button
              key={file.path}
              className={`${styles.fileItem} ${selectedPath === file.path ? styles.active : ""}`}
              onClick={() => onSelect(file.path)}
              title={file.path}
            >
              <span className={`${styles.statusIndicator} ${styles[file.status]}`}>
                {statusChar[file.status] ?? "?"}
              </span>
              <span className={styles.fileName}>{file.path}</span>
            </button>
          ))
        )}
      </div>
    </div>
  );
}
