import styles from "./KnownLimitationsSection.module.css";

interface Props {
  items: string[];
}

/** FM-12 FR-08: Known Limitations 节 */
export function KnownLimitationsSection({ items }: Props) {
  return (
    <ul className={styles.list}>
      {items.map((it, i) => (
        <li key={i} className={styles.item}>
          <span className={styles.bullet} aria-hidden>
            !
          </span>
          <span className={styles.text}>{it}</span>
        </li>
      ))}
    </ul>
  );
}
