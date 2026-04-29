import { useTranslation } from "react-i18next";
import type { MissionInfo } from "../../ipc/commands";
import { MissionListItem, type MissionAction } from "./MissionListItem";
import styles from "./MissionList.module.css";

interface MissionListProps {
  missions: MissionInfo[];
  selectedId: string | null;
  onSelect: (id: string) => void;
  onAction: (id: string, action: MissionAction) => void;
  onNewMission: () => void;
  onImport?: () => void;
}

export function MissionList({
  missions,
  selectedId,
  onSelect,
  onAction,
  onNewMission,
  onImport,
}: MissionListProps) {
  const { t } = useTranslation("mission");
  return (
    <div className={styles.container}>
      <div className={styles.header}>
        <h3 className={styles.title}>{t("title")}</h3>
        <div className={styles.headerActions}>
          {onImport && (
            <button
              className={styles.newBtn}
              onClick={onImport}
              type="button"
              title={t("importMission")}
            >
              <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
                <path d="M14 10v3a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1v-3" />
                <polyline points="4 7 8 11 12 7" />
                <line x1="8" y1="3" x2="8" y2="11" />
              </svg>
            </button>
          )}
          <button
            className={styles.newBtn}
            onClick={onNewMission}
            type="button"
            title={t("newMission")}
          >
            <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round">
              <line x1="8" y1="3" x2="8" y2="13" />
              <line x1="3" y1="8" x2="13" y2="8" />
            </svg>
          </button>
        </div>
      </div>
      <div className={styles.list}>
        {missions.length === 0 ? (
          <div className={styles.empty}>
            <p className={styles.emptyText}>{t("noMissionsTitle")}</p>
            <button className={styles.emptyBtn} onClick={onNewMission} type="button">
              + {t("newMission")}
            </button>
          </div>
        ) : (
          missions.map((m) => (
            <MissionListItem
              key={m.id}
              mission={m}
              selected={m.id === selectedId}
              onSelect={() => onSelect(m.id)}
              onAction={(action) => onAction(m.id, action)}
            />
          ))
        )}
      </div>
    </div>
  );
}
