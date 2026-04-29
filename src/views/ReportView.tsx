import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useUiStore } from "../stores/ui-store";
import { useReportStore } from "../stores/report-store";
import { Button } from "../components/ui";
import {
  ExecSummarySection,
  DecisionsSection,
  EvaluatorReviewSection,
  TaskMatrixSection,
  CostBreakdownSection,
  KnownLimitationsSection,
  LearningFlywheelSection,
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
      { id: "exec-summary", label: "Executive Summary" },
      {
        id: "decisions",
        label: "Architecture Decisions",
        badge: r.decisions.length || undefined,
      },
      {
        id: "evaluator",
        label: "Evaluator Review",
        badge: r.evaluator_review.rounds.length || undefined,
      },
      {
        id: "task-matrix",
        label: "Task Matrix",
        badge: r.task_matrix.length || undefined,
      },
      { id: "cost", label: "Cost Breakdown" },
      {
        id: "limitations",
        label: "Known Limitations",
        badge: r.limitations.length || undefined,
      },
      { id: "learning", label: "Learning Flywheel" },
    ];
  }, [view]);

  // ── Scrollspy + 节折叠
  const [activeSection, setActiveSection] = useState<string>("exec-summary");
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
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
          title="Mission Report"
          subtitle="Select a mission to view its report"
          onClose={handleClose}
          showClose={false}
        />
        <div className={styles.statePanel}>
          <h2 className={styles.stateTitle}>No mission selected</h2>
          <p className={styles.stateDescription}>
            Open a completed mission from <strong>Missions</strong>, then choose
            "View Full Report" to land here.
          </p>
        </div>
      </div>
    );
  }

  if (isLoading && !view) {
    return (
      <div className={styles.container}>
        <Header
          title="Loading report…"
          subtitle={missionId}
          onClose={handleClose}
        />
        <div className={styles.statePanel}>
          <div className={styles.spinner} />
        </div>
      </div>
    );
  }

  if (error && !view) {
    return (
      <div className={styles.container}>
        <Header title="Report unavailable" subtitle={missionId} onClose={handleClose} />
        <div className={styles.statePanel}>
          <h2 className={styles.stateTitle}>Failed to load report</h2>
          <p className={styles.stateError}>{error}</p>
          <Button variant="primary" size="sm" onClick={() => void load(missionId, { force: true })}>
            Retry
          </Button>
        </div>
      </div>
    );
  }

  if (!view) {
    // 没生成过报告
    return (
      <div className={styles.container}>
        <Header title="Mission Report" subtitle={missionId} onClose={handleClose} />
        <div className={styles.statePanel}>
          <h2 className={styles.stateTitle}>Report not generated yet</h2>
          <p className={styles.stateDescription}>
            Aggregating data and writing the executive summary takes up to 30 seconds
            and uses your configured LLM. If no provider is available, a template
            summary will be used instead.
          </p>
          <Button
            variant="primary"
            size="md"
            onClick={() => void handleGenerate()}
            disabled={isGenerating}
          >
            {isGenerating ? "Generating…" : "Generate report"}
          </Button>
        </div>
      </div>
    );
  }

  // ── 正常渲染报告
  const r = view.report;
  return (
    <div className={styles.container}>
      <Header
        title={r.mission.title || "Mission Report"}
        subtitle={`${r.mission.status} · generated ${view.generated_at}`}
        onClose={handleClose}
        right={
          <>
            <Button
              variant="ghost"
              size="sm"
              onClick={() => void handleGenerate()}
              disabled={isGenerating}
              title="Regenerate report from latest data"
            >
              {isGenerating ? "Regenerating…" : "Regenerate"}
            </Button>
          </>
        }
      />

      <div className={styles.body}>
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
              title="Executive Summary"
              collapsed={collapsed.has("exec-summary")}
              onToggle={toggleCollapse}
            >
              <ExecSummarySection mission={r.mission} summary={r.summary} />
            </SectionWrapper>

            <SectionWrapper
              id="decisions"
              title="Architecture Decisions"
              collapsed={collapsed.has("decisions")}
              onToggle={toggleCollapse}
            >
              {r.decisions.length === 0 ? (
                <p className={styles.sectionPlaceholder}>
                  No architecture decisions extracted.
                </p>
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
              title="Evaluator Review"
              collapsed={collapsed.has("evaluator")}
              onToggle={toggleCollapse}
            >
              {r.evaluator_review.rounds.length === 0 ? (
                <p className={styles.sectionPlaceholder}>
                  No Evaluator reviews recorded for this mission.
                </p>
              ) : (
                <EvaluatorReviewSection review={r.evaluator_review} />
              )}
            </SectionWrapper>

            <SectionWrapper
              id="task-matrix"
              title="Task Matrix"
              collapsed={collapsed.has("task-matrix")}
              onToggle={toggleCollapse}
            >
              {r.task_matrix.length === 0 ? (
                <p className={styles.sectionPlaceholder}>No tasks in this mission.</p>
              ) : (
                <TaskMatrixSection rows={r.task_matrix} />
              )}
            </SectionWrapper>

            <SectionWrapper
              id="cost"
              title="Cost Breakdown"
              collapsed={collapsed.has("cost")}
              onToggle={toggleCollapse}
            >
              <CostBreakdownSection breakdown={r.cost_breakdown} />
            </SectionWrapper>

            <SectionWrapper
              id="limitations"
              title="Known Limitations"
              collapsed={collapsed.has("limitations")}
              onToggle={toggleCollapse}
            >
              {r.limitations.length === 0 ? (
                <p className={styles.sectionPlaceholder}>
                  No limitations detected.
                </p>
              ) : (
                <KnownLimitationsSection items={r.limitations} />
              )}
            </SectionWrapper>

            <SectionWrapper
              id="learning"
              title="Learning Flywheel"
              collapsed={collapsed.has("learning")}
              onToggle={toggleCollapse}
            >
              <LearningFlywheelSection data={r.learning_flywheel} />
            </SectionWrapper>
          </div>
        </div>
      </div>
    </div>
  );
}

function Header(props: {
  title: string;
  subtitle?: string;
  onClose: () => void;
  showClose?: boolean;
  right?: React.ReactNode;
}) {
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
            Close
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
