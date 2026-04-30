import { useState, useCallback, useMemo } from "react";
import { useTranslation, Trans } from "react-i18next";
import * as Dialog from "@radix-ui/react-dialog";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { commands } from "../../ipc/commands";
import type { CreateMissionResponse, RepoOrigin } from "../../ipc/commands";
import { Button } from "../ui";
import { tImperative } from "../../i18n";
import styles from "./PlanMissionDialog.module.css";

interface PlanMissionDialogProps {
  open: boolean;
  onClose: () => void;
  /** FM-15 v2.2 (S2-3): mission-first。Step 1 创建 mission 后传 mission_id 给上层。 */
  onPlanReady: (createdMission: CreateMissionResponse) => void;
  onPreflightReady?: (createdMission: CreateMissionResponse) => void;
}

const MAX_CHARS = 2000;
const MAX_TITLE = 80;

type Step = "setup" | "mode";

/** 从描述里截一个像样的 title（FR-18：title 必填，但 plan/sign_contract 完成后会被覆盖）。 */
function deriveTitle(description: string): string {
  const firstLine = description
    .split("\n")
    .map((s) => s.trim())
    .find((s) => s.length > 0) ?? "";
  if (!firstLine) return tImperative("mission:planDialog.untitled");
  return firstLine.length > MAX_TITLE ? firstLine.slice(0, MAX_TITLE) + "…" : firstLine;
}

export function PlanMissionDialog({
  open: isOpen,
  onClose,
  onPlanReady,
  onPreflightReady,
}: PlanMissionDialogProps) {
  const { t } = useTranslation("mission");
  const { t: tc } = useTranslation("common");
  // Step 1 — mission setup
  const [text, setText] = useState("");
  const [origin, setOrigin] = useState<RepoOrigin>("from_scratch");
  const [existingPath, setExistingPath] = useState<string>("");
  const [creating, setCreating] = useState(false);
  const [setupError, setSetupError] = useState<string | null>(null);

  // Step 2 — choose planning mode
  const [step, setStep] = useState<Step>("setup");
  const [createdMission, setCreatedMission] = useState<CreateMissionResponse | null>(null);

  const trimmedText = text.trim();
  const canSubmitSetup = useMemo(() => {
    if (creating) return false;
    if (trimmedText.length === 0) return false;
    if (origin === "from_existing" && existingPath.trim().length === 0) return false;
    return true;
  }, [creating, trimmedText, origin, existingPath]);

  const resetAll = useCallback(() => {
    setText("");
    setOrigin("from_scratch");
    setExistingPath("");
    setCreating(false);
    setSetupError(null);
    setStep("setup");
    setCreatedMission(null);
  }, []);

  const handlePickRepo = useCallback(async () => {
    try {
      const selected = await openDialog({
        directory: true,
        multiple: false,
        title: t("planDialog.pickDirectory"),
      });
      if (typeof selected === "string") {
        setExistingPath(selected);
      }
    } catch (e) {
      console.warn("repo folder pick cancelled or failed:", e);
    }
  }, [t]);

  const handleContinue = useCallback(async () => {
    if (!canSubmitSetup) return;
    setCreating(true);
    setSetupError(null);
    try {
      const result = await commands.createMission({
        title: deriveTitle(trimmedText),
        description: trimmedText,
        repo_origin: origin,
        repo_path: origin === "from_existing" ? existingPath.trim() : undefined,
      });
      setCreatedMission(result);
      setStep("mode");
    } catch (e) {
      setSetupError(String(e));
    } finally {
      setCreating(false);
    }
  }, [canSubmitSetup, trimmedText, origin, existingPath]);

  const handleQuickPlan = useCallback(() => {
    if (!createdMission) return;
    const m = createdMission;
    resetAll();
    onPlanReady(m);
  }, [createdMission, resetAll, onPlanReady]);

  const handlePreflight = useCallback(() => {
    if (!createdMission || !onPreflightReady) return;
    const m = createdMission;
    resetAll();
    onPreflightReady(m);
  }, [createdMission, resetAll, onPreflightReady]);

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (step !== "setup") return;
    if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
      e.preventDefault();
      handleContinue();
    }
  };

  const handleOpenChange = (v: boolean) => {
    if (!v) {
      resetAll();
      onClose();
    }
  };

  // Setup step ------------------------------------------------------
  const renderSetup = () => (
    <>
      <Dialog.Title className={styles.title}>{t("planDialog.step1Title")}</Dialog.Title>
      <p className={styles.subtitle}>{t("planDialog.step1Subtitle")}</p>

      <textarea
        className={styles.textarea}
        value={text}
        onChange={(e) => setText(e.target.value.slice(0, MAX_CHARS))}
        onKeyDown={handleKeyDown}
        placeholder={t("planDialog.descPlaceholder")}
        rows={5}
        autoFocus
      />

      <div className={styles.originSection}>
        <div className={styles.modeLabel}>{t("planDialog.repoSection")}</div>
        <div className={styles.originRow}>
          <label className={`${styles.originOption} ${origin === "from_scratch" ? styles.originOptionActive : ""}`}>
            <input
              type="radio"
              name="repo-origin"
              value="from_scratch"
              checked={origin === "from_scratch"}
              onChange={() => setOrigin("from_scratch")}
            />
            <div className={styles.originOptionBody}>
              <div className={styles.originOptionTitle}>{t("planDialog.fromScratchTitle")}</div>
              <div className={styles.originOptionDesc}>
                <Trans
                  i18nKey="mission:planDialog.fromScratchDesc"
                  components={{ code: <code /> }}
                />
              </div>
            </div>
          </label>
          <label className={`${styles.originOption} ${origin === "from_existing" ? styles.originOptionActive : ""}`}>
            <input
              type="radio"
              name="repo-origin"
              value="from_existing"
              checked={origin === "from_existing"}
              onChange={() => setOrigin("from_existing")}
            />
            <div className={styles.originOptionBody}>
              <div className={styles.originOptionTitle}>{t("planDialog.fromExistingTitle")}</div>
              <div className={styles.originOptionDesc}>{t("planDialog.fromExistingDesc")}</div>
            </div>
          </label>
        </div>

        {origin === "from_existing" && (
          <div className={styles.repoPickerRow}>
            <Button variant="secondary" size="sm" onClick={handlePickRepo}>
              {t("planDialog.pickDirectory")}
            </Button>
            <span
              className={styles.repoPath}
              title={existingPath || ""}
              data-empty={existingPath ? undefined : "true"}
            >
              {existingPath || t("planDialog.noDirChosen")}
            </span>
            {existingPath && (
              <Button variant="ghost" size="sm" onClick={() => setExistingPath("")}>
                {t("planDialog.clear")}
              </Button>
            )}
          </div>
        )}
      </div>

      {setupError && <p className={styles.errorBanner}>{setupError}</p>}

      <div className={styles.footer}>
        <span className={styles.charCount}>
          {text.length}/{MAX_CHARS}
        </span>
        <div className={styles.actions}>
          <Button variant="ghost" size="sm" onClick={() => handleOpenChange(false)} disabled={creating}>
            {tc("cancel")}
          </Button>
          <Button variant="primary" size="sm" onClick={handleContinue} disabled={!canSubmitSetup}>
            {creating ? t("planDialog.creating") : t("planDialog.next")}
          </Button>
        </div>
      </div>
    </>
  );

  // Mode step -------------------------------------------------------
  const renderMode = () => (
    <>
      <Dialog.Title className={styles.title}>{t("planDialog.step2Title")}</Dialog.Title>
      <p className={styles.subtitle}>
        <Trans
          i18nKey="mission:planDialog.step2Subtitle"
          values={{ name: createdMission?.title ?? "" }}
          components={{ strong: <strong /> }}
        />
      </p>

      <div className={styles.repoSummary}>
        <span className={styles.repoSummaryLabel}>
          {createdMission?.repo_origin === "from_scratch"
            ? t("planDialog.newRepo")
            : t("planDialog.existingRepo")}
        </span>
        <span className={styles.repoSummaryPath} title={createdMission?.repo_path ?? ""}>
          {createdMission?.repo_path}
        </span>
      </div>

      <div className={styles.modeSection}>
        <div className={styles.modeCards}>
          {onPreflightReady && (
            <button
              type="button"
              className={`${styles.modeCard} ${styles.preflightCard}`}
              onClick={handlePreflight}
            >
              <div className={styles.cardBadge}>{t("planDialog.preflightBadge")}</div>
              <div className={styles.cardIcon}>💬</div>
              <div className={styles.cardTitle}>{t("planDialog.preflightCardTitle")}</div>
              <div className={styles.cardDesc}>{t("planDialog.preflightCardDesc")}</div>
              <div className={styles.cardMeta}>{t("planDialog.preflightCardMeta")}</div>
            </button>
          )}
          <button
            type="button"
            className={`${styles.modeCard} ${styles.quickCard}`}
            onClick={handleQuickPlan}
          >
            <div className={styles.cardIcon}>⚡</div>
            <div className={styles.cardTitle}>{t("planDialog.quickCardTitle")}</div>
            <div className={styles.cardDesc}>{t("planDialog.quickCardDesc")}</div>
            <div className={styles.cardMeta}>{t("planDialog.quickCardMeta")}</div>
          </button>
        </div>
      </div>

      <div className={styles.footer}>
        <span className={styles.charCount} />
        <Button variant="ghost" size="sm" onClick={() => handleOpenChange(false)}>
          {t("planDialog.decideLater")}
        </Button>
      </div>
    </>
  );

  return (
    <Dialog.Root open={isOpen} onOpenChange={handleOpenChange}>
      <Dialog.Portal>
        <Dialog.Overlay className={styles.overlay} />
        <Dialog.Content className={styles.content}>
          {step === "setup" ? renderSetup() : renderMode()}
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
