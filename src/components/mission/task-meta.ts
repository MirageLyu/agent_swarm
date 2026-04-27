/**
 * FM-15 v2.2 (S4): 解析 TaskInfo / DependencyInfo 上以 JSON 字符串透传的富语义字段，
 * 并提供 Role / Artifact 的 UI 元数据（颜色、图标、显示名）。
 *
 * 后端 `agent::roles::builtin_role_templates()` 是这些数据的"权威源"，本文件做的是
 * 一次显式镜像，避免每次新增 role 都要改前后端两处。
 */

import type { TaskInfo, DependencyInfo } from "../../ipc/commands";

export interface ArtifactDecl {
  /** snake_case 本地名，task 内唯一。 */
  local_name: string;
  /** 业务类型，如 design_doc / code_module / test_module / api_spec / report / config / dataset。 */
  artifact_type: string;
  /** 可选简介，给 UI tooltip。 */
  description?: string | null;
}

export interface FileScopeHints {
  definite: string[];
  possible: string[];
}

export interface RoleMeta {
  id: string;
  displayName: string;
  uiColor: string;
  uiIcon: string;
}

const ROLE_TABLE: Record<string, RoleMeta> = {
  architect: {
    id: "architect",
    displayName: "Architect",
    uiColor: "#a78bfa",
    uiIcon: "\u{1F4D0}",
  },
  implementer: {
    id: "implementer",
    displayName: "Implementer",
    uiColor: "#60a5fa",
    uiIcon: "\u{1F6E0}",
  },
  refactorer: {
    id: "refactorer",
    displayName: "Refactorer",
    uiColor: "#22d3ee",
    uiIcon: "\u{267B}",
  },
  tester: {
    id: "tester",
    displayName: "Tester",
    uiColor: "#34d399",
    uiIcon: "\u{1F9EA}",
  },
  integrator: {
    id: "integrator",
    displayName: "Integrator",
    uiColor: "#fb923c",
    uiIcon: "\u{1F50C}",
  },
  researcher: {
    id: "researcher",
    displayName: "Researcher",
    uiColor: "#94a3b8",
    uiIcon: "\u{1F50D}",
  },
};

const FALLBACK_ROLE: RoleMeta = {
  id: "implementer",
  displayName: "Implementer",
  uiColor: "#60a5fa",
  uiIcon: "\u{1F6E0}",
};

export function getRoleMeta(role?: string | null): RoleMeta {
  if (!role) return FALLBACK_ROLE;
  return ROLE_TABLE[role] ?? FALLBACK_ROLE;
}

/** 列举所有内置 role，供 UI 列表 / 选择器使用，按业务习惯顺序。 */
export const BUILTIN_ROLES: RoleMeta[] = [
  ROLE_TABLE.architect,
  ROLE_TABLE.implementer,
  ROLE_TABLE.refactorer,
  ROLE_TABLE.tester,
  ROLE_TABLE.integrator,
  ROLE_TABLE.researcher,
];

function safeParse<T>(raw: string | null | undefined, fallback: T): T {
  if (!raw) return fallback;
  try {
    const v = JSON.parse(raw);
    return (v ?? fallback) as T;
  } catch {
    return fallback;
  }
}

export function parseAdditionalSkills(task: TaskInfo): string[] {
  return safeParse<string[]>(task.additional_skills_json, []);
}

export function parseProducedArtifacts(task: TaskInfo): ArtifactDecl[] {
  return safeParse<ArtifactDecl[]>(task.produces_artifacts_json, []);
}

export function parseConsumedArtifacts(task: TaskInfo): string[] {
  return safeParse<string[]>(task.consumes_artifacts_json, []);
}

export function parseFileScopeHints(task: TaskInfo): FileScopeHints {
  const fallback: FileScopeHints = { definite: [], possible: [] };
  return safeParse<FileScopeHints>(task.file_scope_hints_json, fallback);
}

export function parseArtifactRefs(dep: DependencyInfo): string[] {
  return safeParse<string[]>(dep.artifact_refs_json, []);
}
