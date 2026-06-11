import { describe, expect, it } from "vitest";
import {
  finishPreflightUiTurn,
  formatPreflightPerfLog,
  markPreflightFirstVisible,
  startPreflightUiTurn,
} from "./preflight-perf";
import type { PreflightPerfSummary } from "../ipc/commands";

const backendPerf: PreflightPerfSummary = {
  backend_prepare_ms: 12,
  llm_first_activity_ms: 90,
  llm_ttft_ms: 140,
  llm_total_ms: 900,
  tool_processing_ms: 33,
  continuation_count: 1,
  turn_total_ms: 1200,
  tool_names: ["add_contract_item", "present_choices"],
  compaction_triggered: false,
  input_tokens: 500,
  output_tokens: 80,
  cache_read_input_tokens: 200,
  cache_creation_input_tokens: 0,
};

describe("preflight perf helpers", () => {
  it("records first visible only once", () => {
    const started = startPreflightUiTurn("choice", 1000);
    const first = markPreflightFirstVisible(started, 1250);
    const second = markPreflightFirstVisible(first, 1500);

    expect(second.firstVisibleAt).toBe(1250);
  });

  it("uses done time as first visible when no earlier feedback exists", () => {
    const started = startPreflightUiTurn("free_input", 1000);
    const summary = finishPreflightUiTurn(started, null, 1800);

    expect(summary.source).toBe("free_input");
    expect(summary.first_visible_ms).toBe(800);
    expect(summary.done_ms).toBe(800);
    expect(summary.backend_perf).toBeNull();
  });

  it("merges backend perf into formatted dev log", () => {
    const started = startPreflightUiTurn("choice", 1000);
    const visible = markPreflightFirstVisible(started, 1300);
    const summary = finishPreflightUiTurn(visible, backendPerf, 2200);
    const log = formatPreflightPerfLog(summary);

    expect(log).toMatchObject({
      source: "choice",
      first_visible_ms: 300,
      done_ms: 1200,
      backend_prepare_ms: 12,
      llm_ttft_ms: 140,
      continuation_count: 1,
      tool_names: ["add_contract_item", "present_choices"],
      input_tokens: 500,
    });
  });
});
