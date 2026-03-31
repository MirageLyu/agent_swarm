import styles from "./PlaceholderView.module.css";

export function MissionsView() {
  return (
    <div className={styles.placeholder}>
      <div className={styles.icon}>
        <svg width="48" height="48" viewBox="0 0 48 48" fill="none">
          <rect
            x="6"
            y="6"
            width="16"
            height="16"
            rx="4"
            stroke="currentColor"
            strokeWidth="2"
          />
          <rect
            x="26"
            y="6"
            width="16"
            height="16"
            rx="4"
            stroke="currentColor"
            strokeWidth="2"
          />
          <rect
            x="6"
            y="26"
            width="16"
            height="16"
            rx="4"
            stroke="currentColor"
            strokeWidth="2"
          />
          <rect
            x="26"
            y="26"
            width="16"
            height="16"
            rx="4"
            stroke="currentColor"
            strokeWidth="2"
          />
        </svg>
      </div>
      <h2 className={styles.title}>Mission Board</h2>
      <p className={styles.description}>Create a new mission to get started</p>
      <button className={styles.primaryBtn}>New Mission</button>
    </div>
  );
}
