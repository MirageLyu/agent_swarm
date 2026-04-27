/**
 * FM-15 v2.2 (S4): 边上的 artifact 数量徽章。
 *
 * 渲染在 DAG 边的中点，提示这条依赖上承载了多少个 artifact。
 * - 0：不渲染。
 * - 1+：圆点 + 数字，hover 上去显示完整 artifact id 列表（`producer.local_name` 形式）。
 *
 * 设计上保持极简——图就够拥挤了，再加大块文字会糊成一片。
 */

import styles from "./ArtifactBadge.module.css";

interface ArtifactBadgeProps {
  /** 边上的 artifact id 列表，形式 `<producer_db_id>.<local_name>`。 */
  artifactRefs: string[];
  /** SVG 中点 x。 */
  x: number;
  /** SVG 中点 y。 */
  y: number;
}

export function ArtifactBadge({ artifactRefs, x, y }: ArtifactBadgeProps) {
  if (artifactRefs.length === 0) return null;

  // 显示 artifact 的"短名"——只取 `.local_name`，避免 UUID 噪声
  const display = artifactRefs
    .map((id) => {
      const idx = id.indexOf(".");
      return idx >= 0 ? id.slice(idx + 1) : id;
    })
    .join(", ");

  return (
    <g className={styles.group} transform={`translate(${x}, ${y})`}>
      <title>{`Artifacts: ${display}`}</title>
      <circle r={9} className={styles.bg} />
      <text className={styles.count}>{artifactRefs.length}</text>
    </g>
  );
}
