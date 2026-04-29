# Miragenty MVP — End-to-End Manual Checklist

> Goal: verify that a fresh user can install the app, configure a provider, and complete a real mission with no developer intervention.
>
> Run this on a clean machine (or after wiping `~/Library/Application Support/com.miragenty.app/`) before tagging an MVP release.

---

## 0. Prerequisites

- [ ] macOS 12+ / Windows 10+ / Linux (webkit2gtk-4.1) machine
- [ ] Working internet connection
- [ ] **An Anthropic API key OR OpenAI-compatible endpoint + key** (DeepSeek, Together, OpenRouter all work)
- [ ] At least 2 GB free disk space (worktree + bundle cache)
- [ ] Git 2.30+ on `PATH`

If running from source instead of installer, also: Node 20+, pnpm 9+, Rust 1.80+.

---

## 1. Install

### From `.dmg` (macOS):
- [ ] Double-click `Miragenty_0.1.0_aarch64.dmg`
- [ ] Drag `Miragenty.app` to `/Applications`
- [ ] First launch: macOS Gatekeeper may show "unidentified developer". Right-click → Open → confirm.
- [ ] App opens to **Missions** view, sidebar visible, no errors in title bar.

### From source:
- [ ] `pnpm install` completes without errors
- [ ] `pnpm tauri build` produces `src-tauri/target/release/bundle/...`
- [ ] Or `pnpm tauri dev` opens the app within 30s

**Acceptance**: app window shows the title "Miragenty Commander" and the Missions list (empty).

**Common failure**:
- macOS Gatekeeper blocks → Right-click → Open
- Linux missing webkit2gtk → `sudo apt install libwebkit2gtk-4.1-0`

---

## 2. Configure provider

- [ ] Click **Settings** in the left sidebar
- [ ] Scroll to **API Keys** section
- [ ] Paste an Anthropic key OR set provider = `openai_compat` + base URL + key
- [ ] Set **Default Model** (e.g. `claude-sonnet-4-5` or `deepseek-chat`)
- [ ] Click **Save**
- [ ] Verify: a green "saved" indicator appears, no error banner

**Acceptance**: returning to Missions view, no "configure API key" warning visible.

---

## 3. Approval policy sanity (FM-14)

- [ ] In Settings, scroll to **Approval Policy** section
- [ ] Verify defaults are visible: timeout 600s, budget warn ratio 0.8, etc.
- [ ] Add `/etc/passwd` to **Protected Paths**, click Save
- [ ] Verify save succeeded (no error)
- [ ] Remove `/etc/passwd`, save again

**Acceptance**: policy edits persist across save without crashing.

---

## 4. Create empty workspace mission

- [ ] Missions view → **+ New Mission**
- [ ] Title: `e2e-test-1`
- [ ] Description: `Create a Python script hello.py that prints "Hello, Miragenty!" and a tests/test_hello.py that asserts the output.`
- [ ] Click **Confirm**
- [ ] When prompted, choose an **empty directory** (e.g. `~/tmp/miragenty-e2e-1`)
- [ ] App should run `git init` automatically; no error.

**Acceptance**: mission appears in the list with status `draft`.

---

## 5. Pre-flight clarification (FM-10)

- [ ] Click the new mission → **Pre-flight** tab
- [ ] Type `Pre-flight` if not already there; chat panel appears
- [ ] Send: `start the clarification`
- [ ] LLM should reply within 10s with clarifying questions
- [ ] Answer 2-3 questions until the StatusBar progress reaches signable
- [ ] Click **Sign Contract**

**Acceptance**: contract status becomes `signed`, scope/constraints/exclusions/assumptions all populated, mission status flips to `preflight` → `draft` (ready to plan).

**Common failure**:
- Stream errors mid-conversation → use the error banner's retry button (DTS-2026-04-08-preflight-streaming)
- LLM keeps asking forever after "100% progress" → check FM-10 status bar logic

---

## 6. Plan mission (FM-01 + FM-07)

- [ ] **Plan Mission** button → confirm dialog → start
- [ ] Watch the planner stream panel: live thinking + tool calls visible
- [ ] After ≤ 90s, DAG canvas renders with 2-5 tasks
- [ ] Each task node shows title, role, complexity badge
- [ ] Drag a task — its dependent edges follow

**Acceptance**: mission status = `planned`, all tasks have status `pending`/`ready`.

---

## 7. Start mission (FM-02 + FM-03 + FM-04)

- [ ] Click **Start Mission**
- [ ] Workspace view automatically opens
- [ ] Each agent starts a worktree under `<workspace>/.worktrees/<agent-id>/`
- [ ] Activity stream shows live tool calls + LLM messages
- [ ] Cost counter increments in the title bar / agent panel

**Acceptance**: at least one agent reaches `running`, activity stream is not silent for > 30s without progress.

---

## 8. Approval queue (FM-14)

- [ ] If your scope involves writing protected paths or running destructive shell commands, the bell icon will light up
- [ ] Click bell → drawer opens → see the pending request
- [ ] **Approve** OR **Reject** with a note
- [ ] Rejection should inject the note back into the agent context as a tool error
- [ ] Verify the agent picks up the note and adjusts in the next step

**Acceptance**: at least one approval round trip works. Skip if your scope didn't trigger any.

---

## 9. Mission completion + delivery (FM-15)

- [ ] All tasks reach `completed`
- [ ] **MissionDeliveryPanel** appears with:
  - [ ] Total tasks count
  - [ ] Total commits on main branch
  - [ ] List of published artifacts with file paths
  - [ ] **Open in Editor / Terminal / Finder** buttons all work
- [ ] In the workspace directory, run `ls`:
  - [ ] You see the actual files (e.g. `hello.py`, `tests/test_hello.py`), NOT just `.git/` and `.worktrees/`
- [ ] Run `git log --oneline`: commits exist with task titles
- [ ] If the panel showed any "AI-resolved conflict" warnings, eyeball those files

**Acceptance**: workspace contains the produced code, mission status = `completed`, no orphan worktrees blocking the next mission.

---

## 10. Mission Report (FM-12)

- [ ] On the Delivery Panel click **View Full Report**
- [ ] Or in Missions list dropdown → **View Report**
- [ ] Report generates within 30s; if no LLM, fallback summary appears within 1s
- [ ] All 7 sections render:
  - [ ] Executive Summary (with metrics row)
  - [ ] Architecture Decisions (cards with Agree/Disagree)
  - [ ] Evaluator Review (timeline)
  - [ ] Task Matrix (table sorted by score)
  - [ ] Cost Breakdown (budget bar + by-model + by-task)
  - [ ] Known Limitations (orange advisories)
  - [ ] Learning Flywheel (purple insight)
- [ ] Click TOC entries → smooth scroll to that section
- [ ] Click section header chevron → collapses with animation
- [ ] Click **Agree** on a decision → button highlights, count persists across re-open
- [ ] Click **Compare Contract** → right-side overlay shows scope items with ✓/✗
- [ ] Click **Export Markdown** → save dialog → choose path → file written → success banner

**Acceptance**: report is informative, votes persist, markdown file opens correctly in any text editor.

---

## 11. Follow-up chat (FM-15.5)

- [ ] In the mission view, scroll to the **Follow-up** chat at the bottom
- [ ] Send: `Add a docstring to hello.py explaining what it prints.`
- [ ] Chat agent responds with a plan, may use `write_file` (with approval)
- [ ] Approve the write
- [ ] Verify `hello.py` has the docstring

OR escalate:

- [ ] Send: `Now also add a CI workflow for this project.`
- [ ] Chat agent should call `propose_followup_mission`
- [ ] Bell lights up → approval drawer shows escalation request
- [ ] Approve → child mission appears in Missions list with parent link

**Acceptance**: at least one of (direct fix / escalation) works end-to-end.

---

## 12. Mission lifecycle (FM-08)

- [ ] In Missions list, dropdown on the test mission → **Re-run (Failed Only)** if anything failed
- [ ] OR **Delete** with "Clean Workspace" → workspace directory + all `.worktrees/` are removed
- [ ] Verify the mission disappears from the list
- [ ] No SQLite lock errors in console

**Acceptance**: lifecycle ops complete without leaving orphan state.

---

## 13. Multi-mission concurrency

- [ ] Create 2 missions in different workspaces
- [ ] Start both
- [ ] Verify they each respect `max_concurrent_agents` (default 3) **per mission**, not globally
- [ ] No worktree path collision

**Acceptance**: both missions complete independently.

---

## 14. Restart from scratch

- [ ] Quit the app
- [ ] Re-launch
- [ ] All previous missions appear, statuses correct
- [ ] Click an old mission → tasks/agents/activity persist
- [ ] Open the report → still readable, votes still there

**Acceptance**: nothing relies on in-memory state.

---

## 15. Diagnostic export (P1, optional)

- [ ] Settings → **Export Diagnostics**
- [ ] Choose a path
- [ ] Verify the bundle contains:
  - [ ] Recent backend log lines (no API keys leaked)
  - [ ] Tauri/Rust version
  - [ ] Mission count
- [ ] Open in any text editor — content is human-readable

**Acceptance**: diagnostic bundle is shareable in a bug report.

---

## Pass criteria

To call MVP "ready for open beta":

- [ ] Sections 1-11 all pass
- [ ] No section requires opening dev tools to recover
- [ ] No section requires editing config files by hand
- [ ] All visible error messages are in the user's language and actionable
- [ ] The 5-minute first-run tour in README matches the actual experience

If any P0 (Sections 1-7, 9-11) fails on a fresh install: **block release**, file DTS, fix, re-run from Section 1.

If only P1 (Sections 12-15) fails: ship anyway, file as known issue in release notes.
