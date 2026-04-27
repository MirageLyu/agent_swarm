import { describe, expect, it } from "vitest";
import {
  parseAdditionalSkills,
  parseArtifactRefs,
  parseConsumedArtifacts,
  parseFileScopeHints,
  parseProducedArtifacts,
  getRoleMeta,
  BUILTIN_ROLES,
} from "./task-meta";
import type { TaskInfo, DependencyInfo } from "../../ipc/commands";

function makeTask(overrides: Partial<TaskInfo> = {}): TaskInfo {
  return {
    id: "t1",
    mission_id: "m1",
    title: "Test",
    description: "",
    status: "pending",
    complexity: "medium",
    assigned_agent_id: null,
    created_at: "",
    completed_at: null,
    ...overrides,
  };
}

function makeDep(overrides: Partial<DependencyInfo> = {}): DependencyInfo {
  return { task_id: "t2", depends_on: "t1", ...overrides };
}

describe("task-meta parsers", () => {
  it("returns sane defaults when JSON fields are absent", () => {
    const t = makeTask();
    expect(parseAdditionalSkills(t)).toEqual([]);
    expect(parseProducedArtifacts(t)).toEqual([]);
    expect(parseConsumedArtifacts(t)).toEqual([]);
    expect(parseFileScopeHints(t)).toEqual({ definite: [], possible: [] });
    expect(parseArtifactRefs(makeDep())).toEqual([]);
  });

  it("parses well-formed JSON fields", () => {
    const t = makeTask({
      additional_skills_json: '["research", "system-design"]',
      produces_artifacts_json:
        '[{"local_name":"api_spec","artifact_type":"api_spec"}]',
      consumes_artifacts_json: '["abc-uuid.api_spec"]',
      file_scope_hints_json:
        '{"definite":["src/auth.ts"],"possible":["src/auth/**.ts"]}',
    });
    expect(parseAdditionalSkills(t)).toEqual(["research", "system-design"]);
    expect(parseProducedArtifacts(t)).toEqual([
      { local_name: "api_spec", artifact_type: "api_spec" },
    ]);
    expect(parseConsumedArtifacts(t)).toEqual(["abc-uuid.api_spec"]);
    expect(parseFileScopeHints(t)).toEqual({
      definite: ["src/auth.ts"],
      possible: ["src/auth/**.ts"],
    });
  });

  it("falls back gracefully on malformed JSON", () => {
    const t = makeTask({ additional_skills_json: "{not-json" });
    expect(parseAdditionalSkills(t)).toEqual([]);
  });

  it("parses artifact refs on dependency", () => {
    const dep = makeDep({ artifact_refs_json: '["t1-uuid.api_spec"]' });
    expect(parseArtifactRefs(dep)).toEqual(["t1-uuid.api_spec"]);
  });
});

describe("role registry mirror", () => {
  it("returns a fallback meta for unknown roles", () => {
    const m = getRoleMeta("unknown-role");
    expect(m.id).toBe("implementer");
  });

  it("has six built-in roles matching the backend RoleRegistry", () => {
    expect(BUILTIN_ROLES.map((r) => r.id)).toEqual([
      "architect",
      "implementer",
      "refactorer",
      "tester",
      "integrator",
      "researcher",
    ]);
  });

  it("each builtin role has color + icon", () => {
    for (const r of BUILTIN_ROLES) {
      expect(r.uiColor.startsWith("#")).toBe(true);
      expect(r.uiIcon.length).toBeGreaterThan(0);
      expect(r.displayName.length).toBeGreaterThan(0);
    }
  });
});
