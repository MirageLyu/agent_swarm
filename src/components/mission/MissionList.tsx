import type { MissionInfo } from "../../ipc/commands";
import { MissionListItem } from "./MissionListItem";
import styles from "./MissionList.module.css";

interface MissionListProps {
  missions: MissionInfo[];
  selectedId: string | null;
  onSelect: (id: string) => void;
  onDelete: (id: string) => void;
}

export function MissionList({
  missions,
  selectedId,
  onSelect,
  onDelete,
}: MissionListProps) {
  return (
    <div className={styles.container}>
      <div className={styles.header}>
        <h3 className={styles.title}>Missions</h3>
      </div>
      <div className={styles.list}>
        {missions.length === 0 ? (
          <p className={styles.empty}>No missions yet</p>
        ) : (
          missions.map((m) => (
            <MissionListItem
              key={m.id}
              mission={m}
              selected={m.id === selectedId}
              onSelect={() => onSelect(m.id)}
              onDelete={() => onDelete(m.id)}
            />
          ))
        )}
      </div>
    </div>
  );
}
