import styles from "./PlaceholderView.module.css";

export function AgentsView() {
  return (
    <div className={styles.placeholder}>
      <h2 className={styles.title}>Agents</h2>
      <p className={styles.description}>Agent activity streams and management</p>
    </div>
  );
}
