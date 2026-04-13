import styles from "./FileScore.module.css";

interface FileScoreProps {
  score: number;
}

function getScoreClass(score: number): string {
  if (score >= 8) return styles.high;
  if (score >= 6) return styles.medium;
  return styles.low;
}

export function FileScore({ score }: FileScoreProps) {
  const displayScore = Math.round(score * 10) / 10;
  return (
    <span className={`${styles.score} ${getScoreClass(score)}`}>
      {displayScore}/10
    </span>
  );
}
