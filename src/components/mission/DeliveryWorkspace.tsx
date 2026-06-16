import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "../ui";
import {
  commands,
  type MissionDeliveryItem,
  type MissionDeliverySnapshot,
  type MissionStatus,
} from "../../ipc/commands";
import { formatBackendError } from "../../i18n/format-error";
import { useUiStore } from "../../stores/ui-store";
import { MissionChatPanel } from "./MissionChatPanel";
import styles from "./DeliveryWorkspace.module.css";

interface DeliveryWorkspaceProps {
  missionId: string;
  missionStatus: Extract<MissionStatus, "completed" | "failed">;
  onFollowupCreated?: (childMissionId: string) => void;
}

type RenderableEntry = string | MissionDeliveryItem;

export function DeliveryWorkspace({ missionId, missionStatus, onFollowupCreated }: DeliveryWorkspaceProps) {
  const { t } = useTranslation("mission");
  const openMissionReport = useUiStore((s) => s.openMissionReport);
  const [snapshot, setSnapshot] = useState<MissionDeliverySnapshot | null>(null);
  const [loading, setLoading] = useState(true);
  const [generating, setGenerating] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const loadDelivery = useCallback(async () => {
    const persisted = await commands.getMissionDelivery(missionId);
    if (persisted) return persisted;

    setGenerating(true);
    await nextTick();
    await commands.generateMissionDelivery(missionId);
    return commands.getMissionDelivery(missionId);
  }, [missionId]);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setGenerating(false);
    setError(null);
    setSnapshot(null);

    loadDelivery()
      .then((loaded) => {
        if (!cancelled) setSnapshot(loaded);
      })
      .catch((err) => {
        if (!cancelled) setError(formatBackendError(err));
      })
      .finally(() => {
        if (!cancelled) {
          setLoading(false);
          setGenerating(false);
        }
      });

    return () => {
      cancelled = true;
    };
  }, [loadDelivery]);

  const warnings = useMemo(() => normalizeEntries(snapshot?.warnings), [snapshot]);
  const hasPrimaryDelivery = Boolean(getPath(snapshot?.primary_delivery) || getLabel(snapshot?.primary_delivery));
  const hasSupportingDeliverables = (snapshot?.supporting_deliverables?.length ?? 0) > 0;
  const shouldWarnNoPackage = !loading && !error && Boolean(snapshot) && !hasPrimaryDelivery && !hasSupportingDeliverables;
  const timeline = useMemo(
    () => normalizeEntries([...(snapshot?.what_changed ?? []), ...(snapshot?.handoff_timeline ?? [])]),
    [snapshot],
  );

  return (
    <div className={styles.workspace} data-testid="delivery-workspace">
      <section className={styles.deliveryCard}>
        <header className={styles.hero}>
          <div>
            <div className={styles.eyebrow}>{t("deliveryWorkspace.eyebrow")}</div>
            <h2 className={styles.title}>{t("deliveryWorkspace.title")}</h2>
            <p className={styles.subtitle}>
              {missionStatus === "failed"
                ? t("deliveryWorkspace.failedSubtitle")
                : t("deliveryWorkspace.completedSubtitle")}
            </p>
          </div>
          {snapshot?.report_id ? (
            <Button variant="primary" size="sm" onClick={() => openMissionReport(missionId)}>
              {t("viewFullReport")}
            </Button>
          ) : null}
        </header>

        {loading ? (
          <div className={styles.state} role="status">
            {generating ? t("deliveryWorkspace.generating") : t("deliveryWorkspace.loading")}
          </div>
        ) : null}

        {error ? (
          <div className={styles.errorBlock} role="alert">
            <strong>{t("deliveryWorkspace.loadErrorTitle")}</strong>
            <span>{error}</span>
          </div>
        ) : null}

        {snapshot ? (
          <div className={styles.contentGrid}>
            <section className={styles.sectionWide}>
              <h3>{t("deliveryWorkspace.sections.overview")}</h3>
              <p>{snapshot.overview || snapshot.result || t("deliveryWorkspace.emptyOverview")}</p>
              {snapshot.result && snapshot.overview ? (
                <p className={styles.resultText}>{snapshot.result}</p>
              ) : null}
            </section>

            {warnings.length > 0 || shouldWarnNoPackage || missionStatus === "failed" ? (
              <section className={styles.sectionWide}>
                <h3>{t("deliveryWorkspace.sections.warnings")}</h3>
                <div className={styles.warningStack}>
                  {missionStatus === "failed" ? (
                    <div className={styles.warning}>{t("deliveryWorkspace.failedWarning")}</div>
                  ) : null}
                  {shouldWarnNoPackage ? (
                    <div className={styles.warning}>{t("deliveryWorkspace.noPackageWarning")}</div>
                  ) : null}
                  {warnings.map((warning, index) => (
                    <div className={styles.warning} key={`warning-${index}`}>
                      {renderEntry(warning)}
                    </div>
                  ))}
                </div>
              </section>
            ) : null}

            <section className={styles.sectionWide}>
              <h3>{t("deliveryWorkspace.sections.primaryDelivery")}</h3>
              {hasPrimaryDelivery && snapshot.primary_delivery ? (
                <DeliverableCard item={snapshot.primary_delivery} primary />
              ) : (
                <div className={styles.empty}>{t("deliveryWorkspace.noPrimaryDelivery")}</div>
              )}
            </section>

            <EntryListSection
              title={t("deliveryWorkspace.sections.howToUse")}
              entries={normalizeEntries(snapshot.how_to_use)}
              empty={t("deliveryWorkspace.emptyHowToUse")}
            />
            <EntryListSection
              title={t("deliveryWorkspace.sections.validation")}
              entries={normalizeEntries(snapshot.validation)}
              empty={t("deliveryWorkspace.emptyValidation")}
            />

            <section className={styles.sectionWide}>
              <h3>{t("deliveryWorkspace.sections.supportingDeliverables")}</h3>
              {snapshot.supporting_deliverables?.length ? (
                <div className={styles.cardList}>
                  {snapshot.supporting_deliverables.map((item, index) => (
                    <DeliverableCard item={item} key={`${getLabel(item)}-${getPath(item)}-${index}`} />
                  ))}
                </div>
              ) : (
                <div className={styles.empty}>{t("deliveryWorkspace.noSupportingDeliverables")}</div>
              )}
            </section>

            <EntryListSection
              title={t("deliveryWorkspace.sections.timeline")}
              entries={timeline}
              empty={t("deliveryWorkspace.emptyTimeline")}
              wide
            />
          </div>
        ) : !loading && !error ? (
          <div className={styles.empty}>{t("deliveryWorkspace.emptySnapshot")}</div>
        ) : null}
      </section>

      <section className={styles.chatShell}>
        <MissionChatPanel missionId={missionId} enabled onFollowupCreated={onFollowupCreated} />
      </section>
    </div>
  );
}

function EntryListSection({
  title,
  entries,
  empty,
  wide = false,
}: {
  title: string;
  entries: RenderableEntry[];
  empty: string;
  wide?: boolean;
}) {
  return (
    <section className={wide ? styles.sectionWide : styles.section}>
      <h3>{title}</h3>
      {entries.length ? (
        <ol className={styles.entryList}>
          {entries.map((entry, index) => (
            <li key={`${renderEntry(entry)}-${index}`}>{renderEntry(entry)}</li>
          ))}
        </ol>
      ) : (
        <div className={styles.empty}>{empty}</div>
      )}
    </section>
  );
}

function DeliverableCard({ item, primary = false }: { item: MissionDeliveryItem; primary?: boolean }) {
  const label = getLabel(item);
  const path = getPath(item);
  const detail = item.summary ?? item.detail ?? item.description ?? null;

  return (
    <article className={primary ? `${styles.deliverable} ${styles.primaryDeliverable}` : styles.deliverable}>
      {label ? <div className={styles.deliverableTitle}>{label}</div> : null}
      {detail ? <p>{detail}</p> : null}
      {path ? <code className={styles.path}>{path}</code> : null}
      {item.status ? <span className={styles.statusPill}>{item.status}</span> : null}
    </article>
  );
}

function normalizeEntries(entries: Array<RenderableEntry> | null | undefined): RenderableEntry[] {
  return entries?.filter(Boolean) ?? [];
}

function getLabel(item: MissionDeliveryItem | null | undefined): string | null {
  return item?.label ?? item?.title ?? item?.name ?? null;
}

function getPath(item: MissionDeliveryItem | null | undefined): string | null {
  return item?.path ?? item?.file_path ?? null;
}

function nextTick(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

function renderEntry(entry: RenderableEntry): string {
  if (typeof entry === "string") return entry;
  return [getLabel(entry), entry.detail ?? entry.summary ?? entry.description, getPath(entry), entry.command, entry.status]
    .filter((part): part is string => Boolean(part))
    .join(" · ");
}
