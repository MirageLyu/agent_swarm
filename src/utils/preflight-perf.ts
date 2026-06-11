import type { PreflightPerfSummary } from "../ipc/commands";

export type PreflightTurnSource = "initial" | "free_input" | "choice" | "mode_switch" | "retry";

export interface PreflightUiTurnState {
  source: PreflightTurnSource;
  startedAt: number;
  firstVisibleAt: number | null;
}

export interface PreflightUiPerfSummary {
  source: PreflightTurnSource;
  first_visible_ms: number | null;
  done_ms: number;
  backend_perf: PreflightPerfSummary | null;
}

export function startPreflightUiTurn(
  source: PreflightTurnSource,
  now: number = Date.now(),
): PreflightUiTurnState {
  return {
    source,
    startedAt: now,
    firstVisibleAt: null,
  };
}

export function markPreflightFirstVisible(
  state: PreflightUiTurnState,
  now: number = Date.now(),
): PreflightUiTurnState {
  if (state.firstVisibleAt !== null) return state;
  return { ...state, firstVisibleAt: now };
}

export function finishPreflightUiTurn(
  state: PreflightUiTurnState,
  backendPerf: PreflightPerfSummary | null,
  now: number = Date.now(),
): PreflightUiPerfSummary {
  const firstVisibleAt = state.firstVisibleAt ?? now;
  return {
    source: state.source,
    first_visible_ms: firstVisibleAt - state.startedAt,
    done_ms: now - state.startedAt,
    backend_perf: backendPerf,
  };
}

export function formatPreflightPerfLog(summary: PreflightUiPerfSummary): Record<string, unknown> {
  const backend = summary.backend_perf;
  return {
    source: summary.source,
    first_visible_ms: summary.first_visible_ms,
    done_ms: summary.done_ms,
    backend_prepare_ms: backend?.backend_prepare_ms ?? null,
    llm_first_activity_ms: backend?.llm_first_activity_ms ?? null,
    llm_ttft_ms: backend?.llm_ttft_ms ?? null,
    llm_total_ms: backend?.llm_total_ms ?? null,
    tool_processing_ms: backend?.tool_processing_ms ?? null,
    continuation_count: backend?.continuation_count ?? null,
    turn_total_ms: backend?.turn_total_ms ?? null,
    tool_names: backend?.tool_names ?? [],
    compaction_triggered: backend?.compaction_triggered ?? false,
    input_tokens: backend?.input_tokens ?? null,
    output_tokens: backend?.output_tokens ?? null,
    cache_read_input_tokens: backend?.cache_read_input_tokens ?? null,
    cache_creation_input_tokens: backend?.cache_creation_input_tokens ?? null,
  };
}
