# Miragenty

> **Harness-driven multi-agent code generation workbench.**
> Tauri + React + Rust desktop app that orchestrates a swarm of LLM coding agents on isolated git worktrees, mediated by a quality harness (pre-flight contract → planner → multi-agent execution → evaluator → mission report).

[![License: GPL-3.0](https://img.shields.io/badge/license-GPL--3.0-blue)](LICENSE)
[![Status: Pre-MVP](https://img.shields.io/badge/status-pre--MVP-orange)](docs/requirements/00-module-overview.md)

> ⚠️ **Pre-MVP**. APIs, schemas, and UI are still moving. Expect bugs. File issues with the diagnostic bundle (`Settings → Export Diagnostics`).

---

## What it actually does

Most "AI swarm" tools are about parallelism. Miragenty's bet is that **parallelism is commoditised, but trustworthy hand-off is not**. The whole product is one quality loop:

```
Pre-flight chat                  → signed Contract (scope/constraints/exclusions/assumptions)
   ↓
Planner Agent (DAG + skills)     → tasks with explicit roles & artifacts
   ↓
Multi-Agent execution            → each agent on its own git worktree
   ↓                                (with live activity stream + cost tracking
   ↓                                 + approval queue for destructive ops)
Evaluator Agent                  → line-level annotations + auto-fix
   ↓
Frontier merge                   → DAG-aware merge into main (3-layer conflict resolver)
   ↓
Mission Report                   → executive summary + decisions + contract diff + markdown export
   ↓
Follow-up chat                   → quick fixes or escalate to a child mission
```

You stay in the loop at the **decisions** that matter (signing the Contract, approving destructive tools, reviewing the report) and out of the loop for everything else.

## Feature status

| Module | Status | What it is |
|---|---|---|
| FM-01 Mission Planning & DAG | ✅ | Planner Agent decomposes mission into tasks; DAG canvas with edit/zoom |
| FM-02 Multi-Agent Orchestration | ✅ | Scheduler + worktree isolation + concurrency limits |
| FM-03 Execution Hardening | ✅ | Checkpoint persistence + schema validation + agent cancel |
| FM-04 Activity Stream & Cost | ✅ | Live event stream + per-agent cost tracking |
| FM-05 Code Review & Diff | ✅ | Monaco editor + worktree diff viewer |
| FM-06 Runtime Intervention | ✅ | Sticky-note injection + checkpoint pause/resume |
| FM-07 Planner UX | ✅ | DAG zoom/pan/fit + planner streaming output |
| FM-08 Mission Lifecycle | ✅ | Delete / stop / restart (full or failed-only) |
| FM-09 UI Polish | 📋 | Topbar metrics, command palette, sidebar agent list (partial) |
| FM-10 Pre-flight & Contract | ✅ | Multi-turn clarification → structured Contract |
| FM-11 Evaluator Agent | ✅ | Auto code review + line annotations + quality scoring + auto-fix |
| FM-12 Mission Report | ✅ | Executive summary + decisions + evaluator diff + markdown export |
| FM-13 Harness Dashboard | ✅ | Lightweight version: cost trend + anomaly detection. Full dashboard (gantt, live polling, ring progress) post-MVP |
| FM-14 Approval Queue | ✅ | Tool/budget/escalation/fetch/chat-commit approval queue with policy config |
| FM-15 Worktree v2.2 | ✅ | Frontier merge + 3-layer conflict resolver + delivery panel |
| i18n | ✅ | Hot-swappable English / 简体中文 across all views; structured backend `IpcError` codes |

**MVP target audience**: open beta. **Current gap**: see [docs/requirements/00-module-overview.md](docs/requirements/00-module-overview.md).

---

## Install

### Prerequisites

| Tool | Minimum | Notes |
|---|---|---|
| **Node.js** | 20+ | Tested on 22.x and 25.x |
| **pnpm** | 9+ | npm/yarn untested; `package-lock.json` is not maintained |
| **Rust toolchain** | 1.80+ (stable) | Install via [rustup](https://rustup.rs/) |
| **Git** | 2.30+ | Worktrees use `git worktree add` |
| **Platform deps** | macOS 12+ / Windows 10+ / Linux (webkit2gtk-4.1) | Tauri 2 prerequisites: <https://v2.tauri.app/start/prerequisites/> |

### From source

```bash
git clone https://github.com/miragelyu/Miragenty
cd Miragenty
pnpm install
pnpm tauri dev          # development mode with HMR
pnpm tauri build        # produces a signed-installable bundle in src-tauri/target/release/bundle/
```

The first build downloads Tauri's CLI bundle and compiles the full Rust dependency tree — expect 5-10 minutes on a clean machine.

> ❗ If `pnpm install` hangs on Mainland China networks, see [.cursor/rules/npm-network.mdc](.cursor/rules/npm-network.mdc) for proxy & SSL workarounds.

---

## First run: a 5-minute tour

### 1. Configure an LLM provider

Open **Settings → API Keys** and paste a key for either:

- **Anthropic** (recommended; tested with `claude-sonnet-4-5` and `claude-opus-4-7`)
- **Any OpenAI-compatible endpoint** (DeepSeek, Together, OpenRouter, local llama.cpp server, etc.)

Set the **default model** in the same panel. The Approval Queue policy and budget defaults are also editable here.

> 🔒 Keys are stored in plaintext in `~/Library/Application Support/com.miragenty.app/config.json` (macOS) / `%APPDATA%/com.miragenty.app/` (Windows). They never leave your machine.

### 2. Create a Mission

1. **Missions** view → **+ New Mission**
2. Type a one-paragraph description (you can iterate via Pre-flight)
3. Choose a workspace directory:
   - **Existing git repo**: Miragenty operates on it directly
   - **Empty directory**: Miragenty runs `git init` for you and treats it as a fresh project
4. (Optional) **Pre-flight chat** to clarify scope, constraints, exclusions, and assumptions. The contract is signed and persisted.
5. **Plan Mission** — the Planner Agent decomposes into a task DAG with roles + artifacts.
6. **Start Mission** — agents claim ready tasks, execute on worktrees, and stream activity to the Workspace view.

### 3. Watch the Approval Queue

The bell icon in the title bar lights up when an agent requests approval (writing to a protected path, running a destructive shell command, hitting a budget threshold, or escalating to a child mission). You can configure protected paths and the budget warn ratio in **Settings → Approval Policy**.

### 4. Read the Mission Report

When all tasks finish, the **Mission Delivery Panel** shows commits + artifacts + a "View Full Report" button. The report has executive summary, architecture decisions (with agree/disagree voting), evaluator review summary, task matrix, cost breakdown, known limitations, and an optional Contract compare overlay. Export to Markdown for archiving.

### 5. Follow up

The follow-up chat at the bottom of the Mission view lets you:
- Ask quick clarifying questions about what was built
- Request a small fix (the chat agent can write/commit directly with your approval)
- Escalate to a child mission with inherited context

> 📋 For a step-by-step verification of every workflow, see [docs/e2e-checklist.md](docs/e2e-checklist.md). Run this before tagging any release.

---

## Architecture at a glance

```
┌──────────────────── Tauri main process (Rust) ────────────────────┐
│                                                                    │
│   commands/   ← IPC layer (≈ 50 commands)                          │
│   agent/      ← AgentEngine, Planner, Evaluator, Scheduler,        │
│                 Approval coordinator + gate, Report generator      │
│   git/        ← WorktreeManager (frontier merge, 3-layer conflict) │
│   tools/      ← Built-in tools (read_file, write_file, shell_exec, │
│                 publish_artifact, fetch_url)                       │
│   llm/        ← Provider abstraction (Anthropic + OpenAI-compat)   │
│   db/         ← SQLite + 22 migrations                             │
│                                                                    │
└────────────────────────────┬───────────────────────────────────────┘
                             │ Tauri IPC (commands + events)
┌────────────────────────────▼───────────────────────────────────────┐
│                                                                    │
│   React 19 + TypeScript + Vite                                     │
│   Zustand for state, Monaco for diff, Radix for primitives         │
│                                                                    │
│   Views: Missions / Preflight / Workspace / Agents / Review /      │
│          Report / Insights / Settings                              │
│                                                                    │
└────────────────────────────────────────────────────────────────────┘
```

Detailed module documentation lives under [`docs/requirements/`](docs/requirements/).

---

## Development

```bash
# Backend
cd src-tauri
cargo check                 # fast type check
cargo test --lib            # 345+ tests, all in-memory
cargo build --release       # release build (~1m on M-series Mac)

# Frontend
pnpm tsc --noEmit           # type check
pnpm lint                   # eslint
pnpm test                   # vitest (unit)
pnpm dev                    # vite dev server (browser fallback, no Tauri IPC)
pnpm tauri dev              # full app with HMR

# Bundle
pnpm tauri build            # creates installer in src-tauri/target/release/bundle/
```

### Project layout

```
miragenty/
├─ src/                       React + TS frontend
│  ├─ views/                  Top-level routes (Missions, Workspace, Report, ...)
│  ├─ components/             Reusable UI (mission, agent, report, approval, ...)
│  ├─ stores/                 Zustand stores
│  └─ ipc/                    Tauri command + event wrappers
├─ src-tauri/                 Rust backend
│  ├─ src/agent/              Engines, planner, evaluator, approval, report
│  ├─ src/commands/           Tauri IPC command handlers
│  ├─ src/db/                 SQLite migrations + queries
│  ├─ src/git/                Worktree manager (frontier merge, conflict resolver)
│  ├─ src/llm/                Provider abstraction
│  └─ src/tools/              Built-in tool executors
├─ docs/
│  ├─ requirements/           IR/SR/AR per FM module + test cases
│  └─ dts/                    Defect tracking sheets
└─ design/prototypes/         HTML mockups that informed the UI
```

### Internationalisation

Miragenty ships with **English** and **Simplified Chinese**, hot-swappable from
**Settings → Language**. The system is built on `react-i18next` and follows a
small set of conventions that contributors should respect:

- **Single source of truth**: the user's language preference lives in the
  backend (`AppConfig.language`, persisted to `config.json`). The frontend
  reads it on startup and writes it back on every switch — `localStorage` is
  intentionally not used to avoid split-brain state between processes.
- **Namespace by user contact surface**, not by code module
  (`common` / `nav` / `settings` / `mission` / `workspace` / `report` /
  `approval` / `insights` / `preflight` / `errors`). Avoid nesting deeper
  than three levels — if you need more, you usually picked the wrong
  namespace.
- **Add a string** by editing both `src/i18n/locales/en-US.json` and
  `src/i18n/locales/zh-CN.json` (English is the fallback, so add it first),
  then call `t('key')` from a `useTranslation('namespace')` hook. Use
  `_one` / `_other` suffixes for pluralisation, and the `<Trans>` component
  when a translated string contains React elements (e.g. `<strong>`).
- **Add a language** by appending the BCP 47 tag to the
  `SUPPORTED` whitelist in `src-tauri/src/commands/config.rs`, copying
  `en-US.json` to `src/i18n/locales/<tag>.json` and translating in place,
  and registering it in `src/i18n/index.ts` (`SUPPORTED_LANGUAGES` +
  `resources`).
- **Backend errors** are returned as structured JSON `IpcError` envelopes
  (`{ code, params, detail }`). The frontend's `formatBackendError` helper
  looks up the `code` in the `errors` namespace and falls back to the raw
  string for legacy errors. Prefer adding a new code over hand-formatting an
  English error string in the backend.

The full design rationale lives in the JSDoc at the top of
[`src/i18n/index.ts`](src/i18n/index.ts).

### Commit convention

[Conventional Commits 1.0](https://conventionalcommits.org/en/v1.0.0/) with project-specific scopes (`agent`, `git`, `db`, `commands`, `ui`, `ipc`, `store`, …). See [.cursor/rules/commit-convention.mdc](.cursor/rules/commit-convention.mdc).

### Reporting bugs

1. **Settings → Export Diagnostics** to dump recent backend logs + IPC traces (sensitive fields redacted)
2. Open a [GitHub issue](https://github.com/miragelyu/Miragenty/issues/new) with:
   - what you were doing
   - what you expected
   - what happened
   - the diagnostic bundle path
3. For defects with sufficient detail, we'll create a DTS in `docs/dts/` and link it back

---

## License

GPL-3.0-only. See [LICENSE](LICENSE).
