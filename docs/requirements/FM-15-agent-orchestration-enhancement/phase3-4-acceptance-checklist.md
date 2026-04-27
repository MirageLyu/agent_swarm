# FM-15 Phase 3 + Phase 4 验收手册

> 适用范围：FM-15 v2.2 Phase 3（Guardrail 完成检测 / Codebase Intelligence / L3 LLM 解冲突 / LLM Judge）+ Phase 4（mission-delivered 交付面板 / Open in Editor 等 / Follow-up Chat + propose_followup_mission）
>
> 阅读对象：没有项目背景的测试人员。
>
> 完成时长：建议预留 **2–3 小时**。
>
> 通过门槛：**M-13 至 M-22 全部勾选 ✅**。其中 M-15、M-17、M-20、M-22 任一失败即视为本阶段不通过。

---

## 0. 名词速查（在 Phase 1 验收手册基础上增加）

| 名词 | 含义 |
|---|---|
| **Guardrail** | 完成检测的"自动检查"：Agent 调用 `task_complete` 后，系统会跑一组 guardrail 决定真完成还是要返工。 |
| **task_complete** | Agent 显式声明完成任务的工具调用；不调用就不算完成。 |
| **Codebase Intelligence** | 在 Agent system prompt 注入 `[Project Structure]` / `[Tech Stack]` / `[Upstream Context]` / `[Base Conflicts]`，让 Agent 不在空气中工作。 |
| **L1 / L2 / L3 合并** | L1=git 自动合并；L2=保守启发式（whitespace-only）；L3=LLM 解冲突。 |
| **LLM Judge** | 一种 guardrail：让另一个 LLM 评判产出是否符合 criteria。 |
| **Frontier Merge** | 没有 completed 后继的 Task 会被合入 main，避免重复合并。 |
| **mission-delivered** | mission 所有 frontier 都成功合入 main 后发出的最终事件，前端用它渲染交付面板。 |
| **Follow-up Chat** | mission 完成后，用户与 Chat Agent 多轮对话；小改动直接 commit，大改动走 propose 升级为子 mission。 |
| **propose_followup_mission** | Chat Agent 评估请求过大时调用的工具；前端弹窗让用户决定升级或拒绝。 |
| **Force Direct** | 用户在 propose 弹窗里点"No, just do it directly"后，Chat 进入强制直接执行模式。 |

---

## 1. 环境准备

> 与 Phase 1 验收完全相同。请先确保你已能：
>
> 1. 启动 Miragenty 桌面端（`pnpm tauri dev`）
> 2. Settings 里配置 LLM API Key + base URL
> 3. 至少跑通过一次 Phase 1 验收（M-01 到 M-12）
>
> 验收 Phase 3+4 需要一个**真实可执行**的 mission：必须有真实代码改动、能跑 git，所以请在 from-scratch 仓库 + 真实小项目两种环境下分别跑一次。

---

## 2. Phase 3 验收点

### M-13. Agent 必须显式调用 task_complete 才算完成（FR-09.3）

**操作步骤**：

1. 新建一个 mission（任意需求，如 "Create a simple Rust hello-world binary that prints args"），repo_origin = from_scratch。
2. 跑 Quick Plan → 确认 → Start Mission，等到至少一个 task 进入 running。
3. 切到 **Workspace** 视图打开 Agent 流，**全文搜索 `task_complete`**。

**预期结果**：

- ✅ 每个 completed 状态的 task，对应的 agent 流里**必定能找到** `tool_use` 是 `task_complete` 的步骤，summary 字段非空。
- ❌ 失败案例：task 显示 completed 但 agent 流里找不到 `task_complete`，或者 agent 输出大段总结性文本就被判完成 → **直接判 M-13 不通过**。

### M-14. Guardrail 失败可注入重试提示

**操作步骤**：

1. 准备一个 mission，给某个 task 在 Planner 阶段编辑后**手动设置一个 produces_artifact**（例如 `api_spec`，类型 `api_spec`），description 写得不要让 Agent 实际产出该 artifact（例如只让它写 README）。
2. 启动该 task。

**预期结果**：

- ✅ Agent 第一次 `task_complete` 后，系统会自动注入一条 "[guardrail] artifact 'xxx' was not published — please publish it via publish_artifact and call task_complete again." 提示。
- ✅ Agent 在重试预算（默认 3）耗尽前能补救则 task → completed；耗尽则 task → failed。
- ✅ Agent 流里能清楚看到 `guardrail` 检查记录 + 重试。

### M-15. Codebase Intelligence 注入（FR-10）⚑ 阻断项

**操作步骤**：

1. 取一个**已有 README + Cargo.toml** 的小型 Rust 项目，from_existing 创建 mission。
2. 设计一个有依赖关系的两节点 DAG：A → B（要求 A 先建一个文件，B 修改 A 建的文件）。
3. 启动执行。

**预期结果**：

- ✅ 在 task A 启动时的 agent system prompt（可在 agent_events / 后端 log 看到）必定包含：
  - `[Project Structure]` 段（一棵 tree -L 3 风格的目录树）
  - `[Tech Stack]` 段（至少识别出 `rust` + `cargo`）
- ✅ 在 task B 启动时，系统 prompt 还要再多两段：
  - `[Upstream Context]`：A 的 completion_summary 和 publish 的 artifact 摘要
  - `[Base Conflicts]`：如果 prepare_task_base 阶段出现合并冲突，这里有按文件分组的 summary（无冲突时写 "No conflicts"）
- ❌ 缺少 `[Project Structure]` 或 `[Tech Stack]` → **判 M-15 不通过**。

### M-16. L3 LLM 解冲突走通（FR-08.2）

**操作步骤**：

1. 设计一个 fan-out → fan-in 的 4 节点 DAG：A → B、A → C、B → D、C → D；让 B 和 C **同时修改同一个文件的同一段**（例如都要在 `src/lib.rs` 的某行写不同 `pub fn` 签名）。
2. mission 配置里 merge_strategy 选择 `llm_resolve`。
3. 启动并等到全部完成。

**预期结果**：

- ✅ 后端日志里能看到 `LlmProviderResolver: resolving conflict` 日志。
- ✅ `merge_records` 表里至少有一行 `final_strategy = 'llm_resolve'`，`llm_resolution_succeeded = 1` 或 `0`（成功或失败都可观察）。
- ✅ 若 LLM 解出来：main 分支上能看到一条额外的 `Merge: LLM-resolved` commit；若失败：日志里有 fallback 到 theirs 的提示，**不能崩溃**。

### M-17. Guardrail::LlmJudge 工作（FR-09.4）⚑ 阻断项

**操作步骤**：

1. 在 Planner 阶段（或人工 SQL 写入）给某个 task 添加一个 LlmJudge guardrail，criteria 写得严格但可达成（例如 `"The README must mention installation steps."`）。
2. 跑这个 task。

**预期结果**：

- ✅ Agent `task_complete` 后，能在 agent_events 看到 `LlmJudge: passed=true reason=...` 或 `passed=false reason=...` 的记录。
- ✅ 当 criteria 故意改成不可达成（例如 `"The output must be in French."` 但 task 实际是英文 README），重试预算耗尽后 task → failed。
- ❌ LlmJudge 永远 pass 或永远 fail / 没有 LLM 调用记录 → **判 M-17 不通过**。

---

## 3. Phase 4 验收点

### M-18. mission-delivered 事件聚合 payload（FR-14.1）

**操作步骤**：

1. 完整跑通一个含 ≥ 2 个 published artifact 的 mission（例如 task A 产出 design_doc，task B 产出 code_module）。
2. 在浏览器 DevTools Console 里执行（开 dev mode 才有）：
   ```js
   window.__TAURI_INTERNALS__.transformCallback // 仅证 dev mode
   ```
3. 等到 mission 状态变为 completed。

**预期结果**：

- ✅ 前端会收到 `mission-delivered` 事件（Console 可见 / 也可在前端 stores 里观察 deliveredPayloads）。
- ✅ 该事件 payload 同时包含：missionId / repoPath / mainBranch / totalTasks / totalCommits / artifacts[].localName / llmResolvedFiles[] / autoResolvedFiles[]。
- ✅ 选中该 mission 的瞬间，DAG 图上方出现 **Mission Delivered** 面板。

### M-19. Open in Editor / Terminal / Finder（FR-14.3）

**操作步骤**：

1. 在已交付的 mission 里点击交付面板的三个按钮各一次。

**预期结果**：

- ✅ "Open in Editor"：默认调系统 `open` 协议（macOS）/`xdg-open`（Linux）/`start`（Windows）打开 repo 目录；若机器上有 VS Code 命令 `code` 也能识别。
- ✅ "Open Terminal"：弹出新的终端窗口，cwd 就是 repo_path。
- ✅ "Reveal in Finder"（macOS） / Explorer（Win）/ Files（Linux）：在文件管理器中高亮该目录。
- ❌ 任意按钮静默失败（无反应、无报错）→ M-19 不通过。

### M-20. LLM-resolved / auto-merged 文件高亮提醒 ⚑ 阻断项

**操作步骤**：

1. 用 M-16 同样的 fan-in 冲突 mission 跑通后查看交付面板。

**预期结果**：

- ✅ 当 mission 包含 LLM 解冲突文件时，交付面板显示橙色警告块 "⚠ N file(s) resolved by AI — please review"，列出每个文件路径。
- ✅ 当 mission 包含被 theirs/启发式自动解决的文件，显示另一块 "⚠ N file(s) auto-merged — verify if needed"。
- ❌ 这两个提示块不显示 / 显示了但文件路径为空 → **判 M-20 不通过**。

### M-21. Chat Agent 处理小改动（FR-15.5）

**操作步骤**：

1. 在已 completed 的 mission 上方滚动，找到 **Follow-up Chat** 面板。
2. 输入：`Add a comment "// hello FM-15" to the top of README.md`，⌘+Enter 发送。
3. 等待返回。

**预期结果**：

- ✅ Chat 流里出现 user / assistant / 流式 token 增量。
- ✅ Assistant bubble 标注 "task_complete"，并附 commit_hash / files_changed=1 / lines_changed≤2。
- ✅ 在终端 `cd <repo_path> && git log --oneline -1` 能看到一条 `chat: ...` commit。
- ✅ 没有触发 propose_followup_mission 弹窗（因为远低于 30 行硬阈值）。

### M-22. propose_followup_mission 流程闭环 ⚑ 阻断项

**操作步骤**：

1. 在同一个 chat 里再发：`Refactor the entire crate into a workspace with frontend / backend / shared subcrates and add full CI`。
2. 等待 chat agent 返回。

**预期结果**：

- ✅ Chat 面板出现橙色提议卡片 "Escalate to a follow-up mission?"，显示 Title / Why / Estimated tasks。
- ✅ 点击 **"Yes, plan it as a new mission"**：
  - 后端创建子 mission（在 missions 表 `parent_mission_id` 指向当前 mission）。
  - 前端自动选中子 mission（左侧列表能看到新条目，状态 draft）。
  - 在子 mission 上手动跑 Plan → 出现完整 DAG。
- ✅ 点击 **"No, just do it directly"**：
  - chat 面板出现一条 system 消息 "[rejected] User declined escalation."。
  - 顶部出现 "direct mode" 徽标。
  - 再发同一句指令 → chat agent 不再调 propose；要么完成（≤30 行）要么 commit_failed/rejected_oversize（>30 行被守门员拒绝）。
- ❌ 弹窗不出现 / 点了"Yes"后没有创建子 mission / 点了"No"后还是弹同样窗 → **判 M-22 不通过**。

---

## 4. 全链路自动化回归

### M-23. 全量后端单测

```bash
cd src-tauri
cargo test --quiet
```

**预期**：`291 passed; 0 failed`（数字可能随后续提交略变；只要全 pass 即可）。

### M-24. 全量前端构建 + 测试

```bash
pnpm tsc --noEmit
pnpm build
pnpm test --run
```

**预期**：三条命令均 exit 0；`pnpm test --run` 全部通过。

---

## 5. 失败时如何收集信息

如果任意 M-XX 不通过：

1. 截图 + 录屏（如果是 UI 问题）。
2. 收集后端日志：`miragenty-*.log`（`~/Library/Logs/...` 或终端 stdout）。
3. 复制 DB 文件：`<data_dir>/miragenty.sqlite`（Settings 里能看到 data_dir 位置）。
4. 提交 issue 时附上：失败用例编号（M-XX） + 复现步骤 + 上述材料。

---

## 6. 通过 Phase 3+4 后还能做什么？

- 试着用真实开源项目（你 GitHub 上的 side project）跑一次 from_existing mission，验证 Codebase Intelligence 在真实仓库下的稳定性。
- 故意在 chat 里发一些"刚好踩在 30 行临界值"的请求，观察 commit_main_workdir 的硬阈值守门员。
- 观察 `merge_records` / `mission_chats` / `task_base_conflicts` 表里随着多次 mission 滚动是否会清理 / 增长合理。
