import styles from "./PlaceholderView.module.css";

export function WorkspaceView() {
  return (
    <div className={styles.placeholder}>
      <h2 className={styles.title}>Workspace</h2>
      <p className={styles.description}>Active agents and task progress will appear here</p>
    </div>
  );
}
