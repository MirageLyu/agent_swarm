import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "../ui";
import {
  commands,
  type DeliveryItem,
  type HowToUseStep,
  type MissionDeliveryView,
  type MissionStatus,
  type ValidationEvidence,
  type ChangeSummary,
} from "../../ipc/commands";
import { formatBackendError } from "../../i18n/format-error";
import { useUiStore } from "../../stores/ui-store";
import { MissionChatPanel } from "./MissionChatPanel";
import styles from "./DeliveryWorkspace.module.css";

interface DeliveryWorkspaceProps {
  missionId: string;
  missionStatus: Extract<MissionStatus, "completed" | "failed">;
  refreshKey?: number;
  onFollowupCreated?: (childMissionId: string, repoPath: string) => void;
}

export function DeliveryWorkspace({
  missionId,
  missionStatus,
  refreshKey = 0,
  onFollowupCreated,
}: DeliveryWorkspaceProps) {
  const { t } = useTranslation("mission");
  const openMissionReport = useUiStore((s) => s.openMissionReport);
  const [delivery, setDelivery] = useState<MissionDeliveryView | null>(null);
  const [loading, setLoading] = useState(true);
  const [generating, setGenerating] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const loadDelivery = useCallback(async () => {
    const persisted = await commands.getMissionDelivery(missionId);
    if (persisted) return persisted;

    setGenerating(true);
    const generated = await commands.generateMissionDelivery(missionId);
    return generated.delivery;
  }, [missionId]);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setGenerating(false);
    setError(null);
    setDelivery(null);

    loadDelivery()
      .then((loaded) => {
        if (!cancelled) setDelivery(loaded);
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
  }, [loadDelivery, refreshKey]);

  const snapshot = delivery?.snapshot ?? null;
  const primaryItem = snapshot?.items[0] ?? null;
  const supportingItems = snapshot?.items.slice(1) ?? [];
  const hasPrimaryDelivery = Boolean(primaryItem?.title || primaryItem?.file_paths.length);
  const shouldWarnNoPackage = !loading && !error && Boolean(snapshot) && !hasPrimaryDelivery;
  const reportId = snapshot?.items.find((item) => item.source === "manifest" && item.title.toLowerCase().includes("report"))?.id;

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
          {reportId ? (
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
              <p>{snapshot.overview.summary || t("deliveryWorkspace.emptyOverview")}</p>
              <p className={styles.resultText}>{snapshot.overview.title}</p>
            </section>

            {snapshot.caveats.length > 0 || shouldWarnNoPackage || missionStatus === "failed" ? (
              <section className={styles.sectionWide}>
                <h3>{t("deliveryWorkspace.sections.warnings")}</h3>
                <div className={styles.warningStack}>
                  {missionStatus === "failed" ? (
                    <div className={styles.warning}>{t("deliveryWorkspace.failedWarning")}</div>
                  ) : null}
                  {shouldWarnNoPackage ? (
                    <div className={styles.warning}>{t("deliveryWorkspace.noPackageWarning")}</div>
                  ) : null}
                  {snapshot.caveats.map((warning, index) => (
                    <div className={styles.warning} key={`warning-${index}`}>
                      {warning}
                    </div>
                  ))}
                </div>
              </section>
            ) : null}

            <section className={styles.sectionWide}>
              <h3>{t("deliveryWorkspace.sections.primaryDelivery")}</h3>
              {primaryItem ? (
                <DeliverableCard item={primaryItem} primary />
              ) : (
                <div className={styles.empty}>{t("deliveryWorkspace.noPrimaryDelivery")}</div>
              )}
            </section>

            <HowToUseSection steps={snapshot.how_to_use} empty={t("deliveryWorkspace.emptyHowToUse")} />
            <ValidationSection entries={snapshot.validation} empty={t("deliveryWorkspace.emptyValidation")} />

            <section className={styles.sectionWide}>
              <h3>{t("deliveryWorkspace.sections.supportingDeliverables")}</h3>
              {supportingItems.length ? (
                <div className={styles.cardList}>
                  {supportingItems.map((item) => (
                    <DeliverableCard item={item} key={item.id} />
                  ))}
                </div>
              ) : (
                <div className={styles.empty}>{t("deliveryWorkspace.noSupportingDeliverables")}</div>
              )}
            </section>

            <ChangeTimeline entries={snapshot.changes} empty={t("deliveryWorkspace.emptyTimeline")} />
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

function DeliverableCard({ item, primary = false }: { item: DeliveryItem; primary?: boolean }) {
  return (
    <article className={primary ? `${styles.deliverable} ${styles.primaryDeliverable}` : styles.deliverable}>
      <div className={styles.deliverableTitle}>{item.title}</div>
      {item.summary ? <p>{item.summary}</p> : null}
      {item.file_paths.map((path) => (
        <code className={styles.path} key={path}>{path}</code>
      ))}
      <span className={styles.statusPill}>{item.confidence}</span>
    </article>
  );
}

function HowToUseSection({ steps, empty }: { steps: HowToUseStep[]; empty: string }) {
  return (
    <section className={styles.section}>
      <h3>How to use</h3>
      {steps.length ? (
        <ol className={styles.entryList}>
          {steps.map((step) => (
            <li key={`${step.title}-${step.detail}`}>
              <strong>{step.title}</strong>
              <p>{step.detail}</p>
            </li>
          ))}
        </ol>
      ) : (
        <div className={styles.empty}>{empty}</div>
      )}
    </section>
  );
}

function ValidationSection({ entries, empty }: { entries: ValidationEvidence[]; empty: string }) {
  return (
    <section className={styles.section}>
      <h3>Validation</h3>
      {entries.length ? (
        <ol className={styles.entryList}>
          {entries.map((entry, index) => (
            <li key={`${entry.status}-${entry.summary}-${index}`}>
              <strong>{entry.status}</strong>
              <p>{entry.summary}</p>
              {entry.command ? <code className={styles.path}>{entry.command}</code> : null}
            </li>
          ))}
        </ol>
      ) : (
        <div className={styles.empty}>{empty}</div>
      )}
    </section>
  );
}

function ChangeTimeline({ entries, empty }: { entries: ChangeSummary[]; empty: string }) {
  return (
    <section className={styles.sectionWide}>
      <h3>What changed</h3>
      {entries.length ? (
        <ol className={styles.entryList}>
          {entries.map((entry) => (
            <li key={`${entry.title}-${entry.detail}`}>
              <strong>{entry.title}</strong>
              <p>{entry.detail}</p>
              {entry.files.map((file) => <code className={styles.path} key={file}>{file}</code>)}
            </li>
          ))}
        </ol>
      ) : (
        <div className={styles.empty}>{empty}</div>
      )}
    </section>
  );
}
