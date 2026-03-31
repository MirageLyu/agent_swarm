import styles from "./PlaceholderView.module.css";

export function SettingsView() {
  return (
    <div className={styles.placeholder}>
      <h2 className={styles.title}>Settings</h2>
      <p className={styles.description}>API keys, model preferences, and project configuration</p>
    </div>
  );
}
