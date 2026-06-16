import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { save } from "@tauri-apps/plugin-dialog";
import { useUiStore } from "../stores/ui-store";
import { useReportStore } from "../stores/report-store";
import { commands } from "../ipc/commands";
import { Button } from "../components/ui";
import {
  ExecSummarySection,
  DecisionsSection,
  EvaluatorReviewSection,
  TaskMatrixSection,
  CostBreakdownSection,
  KnownLimitationsSection,
  LearningFlywheelSection,
  ContractCompareOverlay,
  ReportDeliverySection,
} from "../components/report";
import styles from "./ReportView.module.css";

/**
 * FM-12 Mission Report 视图。
 *
 * 三栏布局（compare 关闭时是两栏）：
 * - 左 220px: TOC + scrollspy
 * - 中: 报告正文（max-width 780px 居中）
 * - 右 320px: Contract 对照面板（Slice 6 实现）
 *
 * 状态机：
 * - 没选 mission → 提示从 Missions 列表进入
 * - 加载中 → spinner
 * - 没生成过报告 → 显示 "Generate report" 按钮
 * - 已生成 → 渲染所有节
 * - 出错 → 错误面板 + 重试按钮
 */

interface SectionDef {
  id: string;
  label: string;
  badge?: string | number;
}

export function ReportView() {
  const { t } = useTranslation("report");
  const { t: tc } = useTranslation("common");
  const missionId = useUiStore((s) => s.activeReportMissionId);
  const openMissionReport = useUiStore((s) => s.openMissionReport);
  const { reports, errors, loadingMissionId, generatingMissionId, load, generate } =
    useReportStore();

  const view = missionId ? reports.get(missionId) : undefined;
  const error = missionId ? errors.get(missionId) : undefined;
  const isLoading = loadingMissionId === missionId;
  const isGenerating = generatingMissionId === missionId;

  // 进入视图自动 load 一次（不强制刷新，缓存命中直接返回）
  useEffect(() => {
    if (!missionId) return;
    void load(missionId).catch(() => {
      // store 已经记录了 error，不需要二次处理
    });
  }, [missionId, load]);

  const handleGenerate = useCallback(async () => {
    if (!missionId) return;
    try {
      await generate(missionId);
    } catch {
      // 错误展示给用户在状态面板
    }
  }, [missionId, generate]);

  const handleClose = useCallback(() => {
    openMissionReport(null);
  }, [openMissionReport]);

  // ── 当前可见的节列表（无报告时给空数组，TOC 不渲染）
  const sections: SectionDef[] = useMemo(() => {
    if (!view) return [];
    const r = view.report;
    return [
      { id: "exec-summary", label: t("tocExecSummary") },
      {
        id: "decisions",
        label: t("tocDecisions"),
        badge: r.decisions.length || undefined,
      },
      {
        id: "evaluator",
        label: t("tocEvaluator"),
        badge: r.evaluator_review.rounds.length || undefined,
      },
      {
        id: "task-matrix",
        label: t("tocTaskMatrix"),
        badge: r.task_matrix.length || undefined,
      },
      { id: "cost", label: t("tocCost") },
      {
        id: "delivery",
        label: "Delivery",
        badge: r.delivery?.items.length || undefined,
      },
      {
        id: "limitations",
        label: t("tocLimitations"),
        badge: r.limitations.length || undefined,
      },
      { id: "learning", label: t("tocLearning") },
    ];
  }, [view, t]);

  // ── Scrollspy + 节折叠 + Contract 对照 + 导出
  const [activeSection, setActiveSection] = useState<string>("exec-summary");
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const [showContract, setShowContract] = useState<boolean>(false);
  const [exporting, setExporting] = useState<boolean>(false);
  const [exportError, setExportError] = useState<string | null>(null);
  const [exportNotice, setExportNotice] = useState<string | null>(null);
  const scrollRef = useRef<HTMLDivElement | null>(null);

  const toggleCollapse = useCallback((id: string) => {
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);

  const handleTocClick = useCallback((id: string) => {
    const el = document.getElementById(`section-${id}`);
    if (!el) return;
    el.scrollIntoView({ behavior: "smooth", block: "start" });
  }, []);

  const handleExport = useCallback(async () => {
    if (!missionId || !view) return;
    setExportError(null);
    setExportNotice(null);
    try {
      const defaultName = `mission-report-${view.report.mission.title
        .toLowerCase()
        .replace(/[^a-z0-9]+/g, "-")
        .slice(0, 40)
        .replace(/(^-|-$)/g, "") || "untitled"}.md`;
      const chosen = await save({
        defaultPath: defaultName,
        filters: [{ name: "Markdown", extensions: ["md"] }],
      });
      if (!chosen) return; // 用户取消
      setExporting(true);
      const res = await commands.exportReportMarkdown({
        mission_id: missionId,
        output_path: chosen,
      });
      setExportNotice(
        t("exportSuccess", { path: `${res.output_path} (${formatBytes(res.bytes_written)})` }),
      );
      setTimeout(() => setExportNotice(null), 5000);
    } catch (err) {
      setExportError(t("exportError", { message: err instanceof Error ? err.message : String(err) }));
    } finally {
      setExporting(false);
    }
  }, [missionId, view, t]);

  // IntersectionObserver 实现 scrollspy。
  // root 必须是滚动容器（.content），不能是默认 viewport，否则永远 false。
  useEffect(() => {
    if (!view || !scrollRef.current) return;
    const root = scrollRef.current;

    // 用一个比较窄的 rootMargin 让"当前可见"集中在视口上 1/4 — 阅读直觉更准
    const observer = new IntersectionObserver(
      (entries) => {
        const visible = entries
          .filter((e) => e.isIntersecting)
          .sort((a, b) => a.boundingClientRect.top - b.boundingClientRect.top);
        if (visible[0]) {
          const id = (visible[0].target as HTMLElement).dataset.sectionId;
          if (id) setActiveSection(id);
        }
      },
      {
        root,
        rootMargin: "0px 0px -70% 0px",
        threshold: 0,
      },
    );

    sections.forEach((s) => {
      const el = document.getElementById(`section-${s.id}`);
      if (el) observer.observe(el);
    });

    return () => observer.disconnect();
  }, [view, sections]);

  // ── 渲染分支
  if (!missionId) {
    return (
      <div className={styles.container}>
        <Header
          title={t("title")}
          onClose={handleClose}
          showClose={false}
        />
        <div className={styles.statePanel}>
          <h2 className={styles.stateTitle}>{t("noReportTitle")}</h2>
          <p className={styles.stateDescription}>{t("noReportBody")}</p>
        </div>
      </div>
    );
  }

  if (isLoading && !view) {
    return (
      <div className={styles.container}>
        <Header title={tc("loading")} subtitle={missionId} onClose={handleClose} />
        <div className={styles.statePanel}>
          <div className={styles.spinner} />
        </div>
      </div>
    );
  }

  if (error && !view) {
    return (
      <div className={styles.container}>
        <Header title={t("title")} subtitle={missionId} onClose={handleClose} />
        <div className={styles.statePanel}>
          <h2 className={styles.stateTitle}>{t("loadError", { message: "" })}</h2>
          <p className={styles.stateError}>{error}</p>
          <Button variant="primary" size="sm" onClick={() => void load(missionId, { force: true })}>
            {tc("retry")}
          </Button>
        </div>
      </div>
    );
  }

  if (!view) {
    return (
      <div className={styles.container}>
        <Header title={t("title")} subtitle={missionId} onClose={handleClose} />
        <div className={styles.statePanel}>
          <h2 className={styles.stateTitle}>{t("noReportTitle")}</h2>
          <p className={styles.stateDescription}>{t("noReportBody")}</p>
          <Button
            variant="primary"
            size="md"
            onClick={() => void handleGenerate()}
            disabled={isGenerating}
          >
            {isGenerating ? t("generating") : t("generate")}
          </Button>
        </div>
      </div>
    );
  }

  // ── 正常渲染报告
  const r = view.report;
  const hasContract = r.contract !== null;

  return (
    <div className={styles.container}>
      <Header
        title={r.mission.title || t("title")}
        subtitle={`${r.mission.status} · ${t("subtitle", { time: view.generated_at })}`}
        onClose={handleClose}
        right={
          <>
            {hasContract && (
              <Button
                variant={showContract ? "primary" : "ghost"}
                size="sm"
                onClick={() => setShowContract((v) => !v)}
                title={t("contractCompare.title")}
              >
                {showContract ? t("hideCompare") : t("compareContract")}
              </Button>
            )}
            <Button
              variant="ghost"
              size="sm"
              onClick={() => void handleExport()}
              disabled={exporting}
              title={t("exportMarkdown")}
            >
              {exporting ? tc("loading") : t("exportMarkdown")}
            </Button>
            <Button
              variant="ghost"
              size="sm"
              onClick={() => void handleGenerate()}
              disabled={isGenerating}
              title={t("regenerate")}
            >
              {isGenerating ? t("generating") : t("regenerate")}
            </Button>
          </>
        }
      />

      {(exportError || exportNotice) && (
        <div className={exportError ? styles.exportBannerError : styles.exportBannerOk}>
          <span>{exportError ?? exportNotice}</span>
          <button
            type="button"
            className={styles.bannerClose}
            onClick={() => {
              setExportError(null);
              setExportNotice(null);
            }}
          >
            ×
          </button>
        </div>
      )}

      <div
        className={`${styles.body} ${
          showContract && hasContract ? styles.bodyWithCompare : ""
        }`}
      >
        <aside className={styles.toc}>
          <ul className={styles.tocList}>
            {sections.map((s) => (
              <li key={s.id}>
                <button
                  type="button"
                  className={`${styles.tocItem} ${
                    activeSection === s.id ? styles.tocItemActive : ""
                  }`}
                  onClick={() => handleTocClick(s.id)}
                >
                  <span>{s.label}</span>
                  {s.badge !== undefined && (
                    <span className={styles.tocBadge}>{s.badge}</span>
                  )}
                </button>
              </li>
            ))}
          </ul>
        </aside>

        <div className={styles.content} ref={scrollRef}>
          <div className={styles.contentInner}>
            <SectionWrapper
              id="exec-summary"
              title={t("execSummaryHeading")}
              collapsed={collapsed.has("exec-summary")}
              onToggle={toggleCollapse}
            >
              <ExecSummarySection mission={r.mission} summary={r.summary} />
            </SectionWrapper>

            <SectionWrapper
              id="decisions"
              title={t("decisionsHeading")}
              collapsed={collapsed.has("decisions")}
              onToggle={toggleCollapse}
            >
              {r.decisions.length === 0 ? (
                <p className={styles.sectionPlaceholder}>{tc("none")}</p>
              ) : (
                <DecisionsSection
                  reportId={view.report_id}
                  missionId={view.mission_id}
                  decisions={r.decisions}
                  votes={view.votes}
                />
              )}
            </SectionWrapper>

            <SectionWrapper
              id="evaluator"
              title={t("evaluatorHeading")}
              collapsed={collapsed.has("evaluator")}
              onToggle={toggleCollapse}
            >
              {r.evaluator_review.rounds.length === 0 ? (
                <p className={styles.sectionPlaceholder}>{tc("none")}</p>
              ) : (
                <EvaluatorReviewSection review={r.evaluator_review} />
              )}
            </SectionWrapper>

            <SectionWrapper
              id="task-matrix"
              title={t("taskMatrixHeading")}
              collapsed={collapsed.has("task-matrix")}
              onToggle={toggleCollapse}
            >
              {r.task_matrix.length === 0 ? (
                <p className={styles.sectionPlaceholder}>{tc("none")}</p>
              ) : (
                <TaskMatrixSection rows={r.task_matrix} />
              )}
            </SectionWrapper>

            <SectionWrapper
              id="cost"
              title={t("costHeading")}
              collapsed={collapsed.has("cost")}
              onToggle={toggleCollapse}
            >
              <CostBreakdownSection breakdown={r.cost_breakdown} />
            </SectionWrapper>

            <SectionWrapper
              id="delivery"
              title="Delivery"
              collapsed={collapsed.has("delivery")}
              onToggle={toggleCollapse}
            >
              <ReportDeliverySection delivery={r.delivery} />
            </SectionWrapper>

            <SectionWrapper
              id="limitations"
              title={t("limitationsHeading")}
              collapsed={collapsed.has("limitations")}
              onToggle={toggleCollapse}
            >
              {r.limitations.length === 0 ? (
                <p className={styles.sectionPlaceholder}>{t("noLimitations")}</p>
              ) : (
                <KnownLimitationsSection items={r.limitations} />
              )}
            </SectionWrapper>

            <SectionWrapper
              id="learning"
              title={t("learningHeading")}
              collapsed={collapsed.has("learning")}
              onToggle={toggleCollapse}
            >
              <LearningFlywheelSection data={r.learning_flywheel} />
            </SectionWrapper>
          </div>
        </div>

        {showContract && hasContract && (
          <ContractCompareOverlay
            contract={r.contract}
            onClose={() => setShowContract(false)}
          />
        )}
      </div>
    </div>
  );
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

function Header(props: {
  title: string;
  subtitle?: string;
  onClose: () => void;
  showClose?: boolean;
  right?: React.ReactNode;
}) {
  const { t } = useTranslation("common");
  const { title, subtitle, onClose, showClose = true, right } = props;
  return (
    <header className={styles.header}>
      <div className={styles.headerTitleWrap}>
        <div className={styles.headerTitle}>{title}</div>
        {subtitle && <div className={styles.headerSubtitle}>{subtitle}</div>}
      </div>
      <div className={styles.headerActions}>
        {right}
        {showClose && (
          <Button variant="ghost" size="sm" onClick={onClose}>
            {t("close")}
          </Button>
        )}
      </div>
    </header>
  );
}

function SectionWrapper(props: {
  id: string;
  title: string;
  collapsed: boolean;
  onToggle: (id: string) => void;
  children: React.ReactNode;
}) {
  const { id, title, collapsed, onToggle, children } = props;
  // 用 ref 测真实高度，让动画从内容高 → 0 平滑过渡
  const bodyRef = useRef<HTMLDivElement | null>(null);
  const [maxHeight, setMaxHeight] = useState<string>("none");

  useEffect(() => {
    if (!bodyRef.current) return;
    if (collapsed) {
      // 先设到当前实测高度，再下一帧设 0，触发 transition
      const h = bodyRef.current.scrollHeight;
      setMaxHeight(`${h}px`);
      requestAnimationFrame(() => {
        setMaxHeight("0px");
      });
    } else {
      const h = bodyRef.current.scrollHeight;
      setMaxHeight(`${h}px`);
      // 动画结束后放开 max-height，允许内容动态变化
      const t = setTimeout(() => setMaxHeight("none"), 300);
      return () => clearTimeout(t);
    }
  }, [collapsed]);

  return (
    <section
      id={`section-${id}`}
      data-section-id={id}
      className={styles.section}
    >
      <button
        type="button"
        className={styles.sectionHeader}
        onClick={() => onToggle(id)}
      >
        <span
          className={`${styles.sectionChevron} ${
            collapsed ? styles.sectionChevronCollapsed : ""
          }`}
        >
          ▾
        </span>
        <h2 className={styles.sectionTitle}>{title}</h2>
      </button>
      <div
        ref={bodyRef}
        className={`${styles.sectionBody} ${
          collapsed ? styles.sectionBodyCollapsed : ""
        }`}
        style={{ maxHeight }}
      >
        {children}
      </div>
    </section>
  );
}
