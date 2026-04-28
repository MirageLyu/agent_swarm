# FM-15 验收手册（Agent Orchestration Enhancement）

> 适用范围：FM-15 v2.2 全量验收 — 覆盖 **Phase 1**（数据模型 + Planner Agent Loop + Pre-flight 整合） / **Phase 2**（增量 worktree + 三层合并） / **Phase 3**（Guardrail / Codebase Intelligence / LLM 解冲突 / LlmJudge） / **Phase 4**（mission-delivered 交付面板 + Chat Agent + propose_followup_mission）以及 **Follow-up**（多层超时看门狗 + 一键重启 + 失败可视化 + shell 实时流）。
>
> 阅读对象：**没有项目背景的测试人员**。本文不假设你了解 Tauri、Rust、React 或 Miragenty 历史架构；按章节顺序操作即可。
>
> 完成时长：建议预留 **3.5–5 小时**（含一次性环境配置）。
>
> 通过门槛：**M-01 至 M-31 全部勾选 ✅**。其中 M-04 / M-07 / M-09 / M-12 / M-15 / M-19 / M-20 / M-23 / M-25 / M-27 / M-31 任一失败即视为本次 FM-15 验收不通过（标 ⚑ 的为阻断项）。

---

## 0. 名词速查（先看 2 分钟）

| 名词 | 含义 |
|---|---|
| **Mission** | 一个交付任务，从需求描述开始，最终生成可执行的子任务图。 |
| **Task** | Mission 内的一个子任务，对应 DAG 上的一个节点。 |
| **DAG** | 任务依赖关系图，节点是 Task，箭头表示"必须先完成箭头起点"。 |
| **Planner** | 把需求拆成 DAG 的 AI Agent。 |
| **Pre-flight** | 在 Planner 之前的多轮对话澄清环节，输出"合同"（Contract）。 |
| **Contract** | 由 Pre-flight 总结出的需求清单，包含 Scope（范围）、Constraints（约束）、Exclusions（排除项）等四类条目。 |
| **Quick Plan** | 跳过 Pre-flight，直接交给 Planner 生成 DAG 的快速通道。 |
| **Role / Skill / Artifact** | 富语义字段：Role 是任务承担的角色（架构师/实现者…），Skill 是可插入的能力包，Artifact 是任务产出的具名物件（如 `api_spec`）。 |
| **Worktree** | 每个 Task 用一个独立的 Git worktree（分支 `agent/<task_id>`）执行，互不干扰。 |
| **Task Base** | Phase 2 概念：调度该 Task 前先把所有已 completed 的父任务合并到 `task-base/<task_id>` 分支，agent 从这个分支派生 worktree，因此能"看到"上游产物。 |
| **L1 / L2 / L3 合并** | L1 = git 自动合并；L2 = 保守启发式（whitespace-only / theirs fallback）；L3 = LLM 解冲突。 |
| **Guardrail** | 完成检测：Agent 调用 `task_complete` 后系统跑一组守门员（ArtifactsExist / CommandPasses / FilesNonEmpty / LlmJudge）决定真完成还是返工。 |
| **task_complete** | Agent 显式声明完成的工具调用。不调就不算完成。 |
| **Codebase Intelligence** | 在 Agent system prompt 注入 `[Project Structure]` / `[Tech Stack]` / `[Upstream Context]` / `[Base Conflicts]`，让 Agent 不在空气中工作。 |
| **mission-delivered** | mission 所有 frontier 都成功合入 main 后发出的最终事件，前端用它渲染交付面板。 |
| **Frontier Merge** | 没有 completed 后继的 Task 才会被合入 main，避免重复合并。 |
| **Follow-up Chat** | mission 完成后的多轮对话面板：小改动直接 commit 到 main，大改动走 propose 升级为子 mission。 |
| **propose_followup_mission** | Chat Agent 评估请求过大时调用的工具；前端弹窗让用户选升级或拒绝。 |
| **Watchdog（多层超时）** | L1 stream-idle（LLM token 静默） / L2 shell idle+wall（shell_exec 子进程） / L3 read-only loop（连续只读工具循环） / L4 wall-clock（agent 兜底）。 |
| **agent-tool-stream** | Follow-up 加的实时事件：shell_exec 的 stdout / stderr / meta 字节增量，前端 Workspace 拼接显示。 |

---

## 1. 环境准备（一次性，做一次就好）

### 1.1 系统要求

- macOS 12+ / Windows 10+ / Linux x64
- 已安装：
  - Node.js ≥ 20，`pnpm` ≥ 9（运行 `pnpm -v` 应能显示版本号）
  - Rust toolchain ≥ 1.78（运行 `cargo --version`）
  - Git ≥ 2.30（运行 `git --version`）
  - SQLite 命令行工具 `sqlite3`（macOS 自带；Linux `apt install sqlite3`；Windows 装 [precompiled binary](https://www.sqlite.org/download.html)）
- 一个能联网的环境（首次启动需要拉依赖、Planner / Agent 调用 LLM）

### 1.2 准备一份 LLM API Key

测试默认使用 **DashScope**（阿里云通义千问 OpenAI 兼容接口）：

1. 申请 Key：https://dashscope.console.aliyun.com/apiKey
2. Key 形如 `sk-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx`，请妥善保存。

### 1.3 安装与启动

```bash
# 1) 拉代码（已克隆可跳过）
git clone <repo> Miragenty && cd Miragenty

# 2) 安装前端依赖
pnpm install

# 3) 启动 Miragenty（首次会编译 Rust，耐心等 1–3 分钟）
RUST_LOG=miragenty=debug pnpm tauri dev
```

启动后会弹出一个名为 **Miragenty** 的桌面窗口。**保留这个终端窗口**，验收过程中要看 Rust 日志。

### 1.4 配置 LLM Key

1. 在 Miragenty 左侧导航点击 **Settings**（齿轮图标）。
2. 在 `Provider` 选择 `openai_compat`（默认即此值）。
3. `Base URL` 填 `https://dashscope.aliyuncs.com/compatible-mode/v1`（默认即此值）。
4. `Default model` 填 `qwen3.5-plus`（默认即此值）。
5. 在 `API Keys`，找到 `dashscope` 这一行，把 1.2 拿到的 Key 粘进去，点 **Save**。
6. 提示 "Configuration saved. ..." 后回到 **Missions** 页面。

> 如果你的 Key 是 OpenAI、Claude、DeepSeek 等其他厂商，请同步改 Base URL 和模型名。验收脚本本身与具体 Provider 无关。

### 1.5 准备测试目录

打开**新的**终端（不要关闭 1.3 的窗口），执行：

```bash
# A) "已有仓库"测试目标 — Phase 1 路径 B / Phase 3 M-15 用得上
mkdir -p ~/miragenty-fm15-test/existing-repo
cd ~/miragenty-fm15-test/existing-repo
git init -q
mkdir -p src/auth src/legacy
echo 'export function login() { return "TODO"; }' > src/auth/login.ts
echo 'export function legacyPay() { return "DONT_TOUCH"; }' > src/legacy/payment.ts
echo '# Existing project' > README.md
git add . && git commit -q -m "init"

# B) 一个真实的小型 Rust 项目 — Phase 3 M-15 / M-19 用
mkdir -p ~/miragenty-fm15-test/rust-mini
cd ~/miragenty-fm15-test/rust-mini
cargo init --bin -q
git add . && git commit -q -m "init"

# C) from_scratch 路径无需手动准备，Miragenty 会自动在 ~/miragenty-workspaces/<slug>/ 下建目录
```

### 1.6 找到数据库路径（验收过程中要查 SQLite）

| 操作系统 | 路径 |
|---|---|
| macOS | `~/Library/Application Support/com.miragenty.app/miragenty.db` |
| Linux | `~/.local/share/com.miragenty.app/miragenty.db` |
| Windows | `%APPDATA%\com.miragenty.app\miragenty.db` |

之后凡是说「打开 DB 查表」，都是指对这个文件运行 `sqlite3`：

```bash
sqlite3 "~/Library/Application Support/com.miragenty.app/miragenty.db"
# 进去后用 .tables 查表，用 SELECT 查内容，用 .quit 退出
```

> ⚠️ Miragenty 在跑时持有数据库锁。要在 Miragenty 之外查表，**先关 Miragenty 再查**，或者在另一个 shell 里用 `sqlite3 -readonly` 打开。

---

## 2. 自动化基线（30 秒自检）

在 1.3 的项目根目录另开一个终端跑：

```bash
cargo test --lib --manifest-path src-tauri/Cargo.toml 2>&1 | tail -3
pnpm tsc --noEmit
pnpm test --run 2>&1 | tail -5
pnpm build 2>&1 | tail -5
```

### M-01 自动化测试基线

| 项 | 内容 |
|---|---|
| **预期** | 四条命令依次输出："**294 passed; 0 failed**"（数字可能随后续提交略变，只要全 pass 即可）、TypeScript 无任何输出（即 0 错）、`pnpm test --run` 全部通过、`pnpm build` 在 ≤ 3s 内输出 `built in <2s`（数字看机器性能，不报错即可）。 |
| **若失败** | 整套 FM-15 验收不通过——开发同学先修 baseline 再继续。 |
| **通过?** | ☐ |

---

## 3. Phase 1 路径 A：Quick Plan（直接 Planner，from_scratch）

### 3.1 操作步骤

1. 在 Miragenty 左上角点 **+ New Mission**。
2. 弹出对话框，标题 **New Mission**：
   - 在大文本框输入：`实现一个用户认证系统：邮箱注册、密码登录、密码重置。`
   - **项目仓库** 区域：选 **从零开始**（默认就是这个）。
3. 点 **下一步 →**。等 1–3 秒，对话框换第二步。
4. 第二步标题 **选择启动方式**，看到两张卡片：
   - 左卡：**💬 Pre-flight 澄清**（带"推荐"badge）
   - 右卡：**⚡ Quick Plan**
5. **点右边那张 Quick Plan**。

### M-02 Mission 创建并落库

| 项 | 内容 |
|---|---|
| **预期** | 对话框关闭，回到 Mission 列表，看到刚创建的 Mission（标题前 6–10 字会自动从描述里截取）。 |
| **DB 验证** | `sqlite3 …miragenty.db "SELECT id, title, repo_origin, repo_path, status FROM missions ORDER BY created_at DESC LIMIT 1;"` ——应输出一行：`repo_origin=from_scratch`、`repo_path` 形如 `/Users/<you>/miragenty-workspaces/<slug>`、`status` 为 `draft` 或 `planning`。 |
| **文件系统验证** | 上述 `repo_path` 目录真实存在，且执行 `git -C <repo_path> log --oneline` 至少有 1 条 commit（初始提交）。 |
| **通过?** | ☐ |

### 3.2 观察 Planner Agent Loop

点 Quick Plan 后，主区域应立刻出现两个面板：

- 上方 **PlannerStreamPanel**：以原始文本形式逐字打印 LLM 思考过程。
- 下方 **Planner Agent Loop** 面板：以**步骤卡片**形式展示，每步是一个圆角块，左上角带步号、工具名（如 `propose_task` / `add_dependency` / `validate_plan` / `finalize_plan`），右侧是耗时和 token 数。

整个过程通常持续 **20–90 秒**，结束后 Mission 状态自动变为 `draft`，DAG 出现在主画布。

### M-03 Planner Agent Loop 透传

| 项 | 内容 |
|---|---|
| **预期 ①** | 下方 **Planner Agent Loop** 面板在点击 Quick Plan 后 **3 秒内** 出现至少 1 个步骤卡片（不是空白等好几十秒）。 |
| **预期 ②** | 看到的工具名至少包括 `propose_task`、`add_dependency`、`validate_plan`、`finalize_plan` 这几种（可能还有 `list_skills` / `read_file` / `list_directory`，正常）。 |
| **预期 ③** | 终端日志（1.3 那个窗口）出现 `planner_engine` 相关行，最后一行类似 `complete_planner_session` 且 `total_steps` > 0、`total_tokens` > 0。 |
| **通过?** | ☐ |

### M-04 ⚑ DAG 富语义渲染（核心验收）

Planner 完成后，主画布显示 DAG。**鼠标悬停** 在任意节点上，会弹出 tooltip。

| 项 | 内容 |
|---|---|
| **预期 ①（节点徽章）** | 每个节点 **卡片左下角** 都有一个圆角胶囊形状的 **Role 徽章**：emoji + 中文/英文名。Architect 紫色（📐）、Implementer 蓝色（🛠）、Tester 绿色（🧪）、Refactorer 青色（♻️）、Integrator 橙色（🔌）、Researcher 灰色（🔍）。**至少出现 2 种不同 Role**（一个 3 task 的 Mission 经常是 architect + 2 implementer）。 |
| **预期 ②（产出徽章）** | 至少有 1 个节点在 Role 徽章右侧多一个 `✨ N` 形 pill（N ≥ 1），代表它声明了 N 个 produces_artifacts；hover 这个 pill 自带 OS-level tooltip 显示 `local_name · artifact_type`。 |
| **预期 ③（节点 hover tooltip）** | 节点 tooltip 头部显示完整 Role 徽章（带文字 "Implementer" 等），下方依次出现：描述、`Expected: …`（一行需求验收语）、虚线框包住的 Produces / Consumes / Skills 三类 chip 区块（不要求三类都出现，但至少出现一类的 Mission 应当 ≥ 70%）、最后一行 `Status: pending`。 |
| **预期 ④（边上的 ArtifactBadge）** | 如果 DAG 至少有一条依赖边，且 Planner 写了 `consumes_artifacts`：那条边的中点会有一个**蓝色描边小圆点 + 数字**。鼠标 hover 圆点显示 `Artifacts: <local_name>, …`。**对于 Quick Plan 的简单需求，可以接受 0 条边带 ArtifactBadge——只要 M-09 在 Pre-flight 路径下通过即可**。 |
| **DB 验证** | `sqlite3 …miragenty.db "SELECT id, role, expected_output, additional_skills, produces_artifacts, file_scope_hints FROM tasks WHERE mission_id='<上一步的 mission id>';"`：每一行 `role` 非空（最差也是 `implementer`），`expected_output` 非空字符串，`produces_artifacts` 是合法 JSON 数组（至少有 1 行不是 `[]`），`file_scope_hints` 是 `{"definite":[...], "possible":[...]}` 的 JSON。 |
| **通过?** | ☐ |

### M-05 状态流转

| 项 | 内容 |
|---|---|
| **操作** | 在 DAG 上方找到 **Confirm** 按钮（仅 `status=draft` 时显示）。点一下。 |
| **预期** | Mission 状态变 `planned`；DAG 上**根任务**（没有箭头指进来的任务）的状态圆圈从空心圆 ⭕ 变成带刻度的 ⊙（`ready`）。 |
| **DB 验证** | `SELECT status FROM missions WHERE id='<id>'` 返回 `planned`；`SELECT status FROM tasks WHERE mission_id='<id>' AND id NOT IN (SELECT task_id FROM task_dependencies)` 应全部返回 `ready`。 |
| **通过?** | ☐ |

> Phase 1 验收到此为止；要不要继续点 **Start** 真正执行，决定是否进入 Phase 2 验收。

---

## 4. Phase 1 路径 B：Pre-flight + Planner（from_existing）

### 4.1 操作步骤

1. 再次点 **+ New Mission**。
2. 大文本框输入：`在已有项目里加一个密码重置流程，复用现有的邮件发送能力，但不要动 src/legacy。`
3. **项目仓库** 选 **已有仓库**。
4. 点出现的 **选择目录…** 按钮，选 1.5 创建的 `~/miragenty-fm15-test/existing-repo`，然后点 **下一步 →**。
5. 第二步选 **💬 Pre-flight 澄清**。
6. 进入 Pre-flight 视图（标题区会有 "Pre-flight Mission"，分左右两栏：左是对话，右是 Contract 面板）。
7. **跟 AI 对话 2–3 轮**：每轮点它给出的选项按钮，或者输入简短回答（如"邮件用现有 SMTP"、"密码强度 ≥ 8 位含大小写和数字"）。
8. 当右栏 Contract 面板的 **Scope** 列出至少 2 条、**Exclusions** 至少包含一条提到 `legacy` 或 `不要动 src/legacy` 的条目时，点 **Sign Contract** 按钮（在 Contract 面板下方）。
9. Sign 之后页面出现 **Pre-flight planner** 标题的 PlannerLoopPanel（与 Quick Plan 相同的步骤卡片样式）。等它跑完（30–120 秒），自动跳回 Missions 视图，DAG 已生成。

### M-06 Mission Contract 落库

| 项 | 内容 |
|---|---|
| **DB 验证 ①** | `sqlite3 …miragenty.db "SELECT mission_id, signed_at FROM mission_contracts ORDER BY created_at DESC LIMIT 1;"`：返回 1 行，`signed_at` 非 NULL（ISO 时间）。 |
| **DB 验证 ②** | `sqlite3 …miragenty.db "SELECT section, text FROM contract_items WHERE contract_id=(SELECT id FROM mission_contracts ORDER BY created_at DESC LIMIT 1);"`：至少有 2 行 `section=scope` 和 1 行 `section=exclusions`，且 `exclusions` 里有一行包含 `legacy`（不区分大小写也行，AI 用什么写法都可以）。 |
| **通过?** | ☐ |

### M-07 ⚑ Pre-flight 路径的 Planner 也跑 Agent Loop

| 项 | 内容 |
|---|---|
| **预期 ①** | Sign Contract 后，**立刻**（≤3 秒）页面下方出现 **Pre-flight planner** 标题的步骤卡片面板（不是空白）。 |
| **预期 ②** | 与 Quick Plan 相同的工具名集合（`propose_task` 等）至少出现一次。 |
| **预期 ③** | 终端日志至少出现一次形如 `[planner_engine] ... kind="preflight"` 的行（具体格式以代码为准，关键字 `preflight` 出现即可）。 |
| **预期 ④** | DB：`SELECT kind, status FROM planner_sessions WHERE mission_id='<id>' ORDER BY created_at DESC LIMIT 1;` 返回 `kind=preflight, status=completed`。 |
| **通过?** | ☐ |

### M-08 Planner 真的看了代码（codebase grounding）

| 项 | 内容 |
|---|---|
| **DB 验证** | `sqlite3 …miragenty.db "SELECT tool_name, tool_args FROM planner_steps WHERE session_id=(SELECT id FROM planner_sessions WHERE mission_id='<id>' ORDER BY created_at DESC LIMIT 1) AND tool_name IN ('read_file','list_directory','search_files');"`：**至少返回 1 行**（典型 3–8 行）。这意味着 Planner 真的探索了你 1.5 准备的 `existing-repo` 目录。 |
| **节点 file_scope_hints 验证** | `SELECT id, file_scope_hints FROM tasks WHERE mission_id='<id>';`：**至少 1 个 task** 的 `file_scope_hints` 中，`definite` 或 `possible` 列表里出现真实路径（如 `src/auth/login.ts`），而不是凭空捏造的全 `[]`。 |
| **通过?** | ☐ |

### M-09 ⚑ Contract 富语义穿透 DAG

| 项 | 内容 |
|---|---|
| **预期 ①（DAG 节点 Role 多样化）** | 至少出现 2 种 Role（密码重置类需求，典型 architect → implementer → tester 三角色组合）。 |
| **预期 ②（边带 ArtifactBadge）** | 至少有 **1 条依赖边** 上出现蓝色 ArtifactBadge（小圆点 + 数字 ≥1），hover 显示 `Artifacts: <local_name>`，且这个 local_name 形如 `api_spec`、`design_doc`、`test_module` 等。 |
| **预期 ③（task tooltip 显示 Consumes）** | 被 Badge 指向的下游 task，hover 展开 tooltip 后能看到 `Consumes:` 区块，里面有该 artifact local_name。 |
| **DB 验证** | `SELECT task_id, depends_on, artifact_refs FROM task_dependencies WHERE task_id IN (SELECT id FROM tasks WHERE mission_id='<id>');`：至少 1 行 `artifact_refs` 不是 `[]`，且其内容是 `["<uuid>.<local_name>"]` 形式。 |
| **通过?** | ☐ |

### M-10 Scope/Exclusions Guardrail 信号

> 这是 Phase 1 加的轻量 Guardrail。它**不阻止** finalize，但应该在 `validate_plan` 工具调用结果里以 warn 形式出现。

| 项 | 内容 |
|---|---|
| **DB 验证（scope 覆盖）** | `sqlite3 …miragenty.db "SELECT tool_name, tool_result FROM planner_steps WHERE session_id=(SELECT id FROM planner_sessions WHERE mission_id='<id>' ORDER BY created_at DESC LIMIT 1) AND tool_name='validate_plan';"`：返回 ≥ 1 行。如果 LLM 一次就把每条 scope 覆盖好，`tool_result` 中可能没有 warn——这是正面情况。如果有 warn，应当看到形如 `"code":"WARN_SCOPE_NOT_COVERED"` 或 `"code":"WARN_EXCLUSION_TOUCHED"` 的字段，且 `"severity":"warn"`。 |
| **故意触发实验（可选，10 分钟）** | 1) 重新建一个 from_existing Mission，需求写成"重写 src/legacy 模块"。2) Pre-flight 时，故意在 Exclusions 里加一条"不要修改 src/legacy"。3) Sign 后观察 PlannerLoopPanel 中 `validate_plan` 的 result。**预期**：要么 LLM 自己改任务路径避开冲突，要么至少出现一次 `WARN_EXCLUSION_TOUCHED`。无论哪一种，最终 finalize 仍能成功（warn 不阻塞）。 |
| **通过?** | ☐ |

---

## 5. Phase 1 路径 C：fetch_url 工具的用户确认

> 这一节验证 Planner 调用 `fetch_url` 工具时，前端弹窗能正确显示并按用户决策放行 / 拦截。本节**最容易在没有外网时跳过**，不勾选不影响 FM-15 通过，但若有外网建议跑一遍。

### 5.1 操作步骤

1. 新建一个 Mission，需求写：`实现 OAuth2 登录；请先用 fetch_url 阅读 https://oauth.net/2/ 上 Authorization Code Grant 节，然后再设计任务。`
2. 仓库选 **从零开始** → **下一步 →** → **Quick Plan**。
3. Planner 跑到某一步时，画面会**弹出一个深色叠加层 dialog**，标题为 "Planner wants to fetch a URL"。

### M-11 fetch_url 用户确认

| 项 | 内容 |
|---|---|
| **预期 ①** | Dialog 显示要访问的 URL（`https://oauth.net/...`）、host、reason（LLM 给的访问理由）。下方三个按钮：**Allow once** / **Allow this session** / **Deny**。 |
| **预期 ②** | 点 **Deny**：dialog 关闭，PlannerLoopPanel 中下一步的 `tool_result` 含 `denied` 字样，Planner 自行调整不再访问该 URL。 |
| **预期 ③** | 重做一次 5.1，这次点 **Allow once**：fetch 成功，可在 `planner_steps` 表的 `tool_result` 字段看到非空响应预览。 |
| **预期 ④（黑名单）** | 让 LLM fetch `http://localhost:8080/` 或 `http://192.168.1.1/`：**不应弹窗**，Planner 直接收到 `host blocked` 错误（从 `planner_fetch.rs` 黑名单逻辑），日志中能看到 `HostBlocked`。 |
| **通过?** | ☐ |

> 触发 ④ 比较难（需要 LLM 主动尝试 localhost），可改用单元测试佐证：`cargo test --lib planner_fetch::tests::blocklist_localhost_and_private_ip` 应该已经在 M-01 中通过。

---

## 6. Phase 1 收尾

### M-12 ⚑ 数据完整性 & 无回归

| 项 | 内容 |
|---|---|
| **预期 ①（无报错）** | 退出 Miragenty（关窗口），回看 1.3 终端：搜索 `ERROR ` 关键字（注意大小写）。**预期**：除上面故意触发的 fetch deny / host blocked 之外，没有任何未捕获 ERROR；尤其不应有 `panicked at`、`thread '...' panicked` 字样。 |
| **预期 ②（DB schema）** | `sqlite3 …miragenty.db ".tables"` 应包含：`missions`, `tasks`, `task_dependencies`, `mission_contracts`, `contract_items`, `preflight_sessions`, `planner_sessions`, `planner_steps`, `planner_session_fetch_grants`, `artifacts`, `task_base_conflicts`, `merge_records`, `mission_chats`。一张都不能少。 |
| **预期 ③（tasks schema）** | `sqlite3 …miragenty.db "PRAGMA table_info(tasks);"` 应包含字段 `role`, `expected_output`, `additional_skills`, `produces_artifacts`, `consumes_artifacts`, `file_scope_hints`, `guardrails`, `actual_files_modified`, `last_error`, `last_failed_at`。`PRAGMA table_info(task_dependencies);` 应包含 `artifact_refs`。 |
| **预期 ④（重启不丢数据）** | 关 Miragenty 再 `pnpm tauri dev` 启动一次：之前创建的 missions 全部出现在左侧列表，点开 DAG 仍能看到 RoleBadge / ArtifactBadge。 |
| **通过?** | ☐ |

---

## 7. Phase 2：增量 Worktree + 三层合并

> Phase 2 的关键是 **每个 Task 不再从 main 凭空起步，而是从"当前 Task 之前所有 completed 父任务的合并结果"派生 worktree**；Mission 跑完后再按拓扑序合入 main，过程中遇冲突按 L1/L2/L3 三层逐级降级解决。

### 7.1 准备一个会触发增量 worktree 的 mission

需求：在 `from_scratch` 模式下让 Planner 出一个**至少 3 节点、至少 1 条依赖边**的 DAG。如下文本通常足够：

```
搭一个最小 Express 服务：先建 package.json + server.ts 骨架；再加 /health 路由；最后写一个 README 介绍如何跑起来。
```

走完 Quick Plan → Confirm → **Start** → 等待全部 task 进入 `running` → `completed`（约 5–15 分钟）。

### M-13 增量 Worktree 落库 + 事件透传

| 项 | 内容 |
|---|---|
| **DB 验证 ①** | `sqlite3 …miragenty.db "SELECT task_id, parent_count, conflict_count, layer_summary FROM task_base_conflicts ORDER BY created_at;"`：**至少有 1 行**（依赖任务跑前一定会写）；对叶子任务 `parent_count` ≥ 1，`layer_summary` 形如 `"auto"` / `"heuristic_theirs"` / `"llm_resolve"`。 |
| **文件系统验证** | `git -C <repo_path> branch -a`：能看到 `task-base/<task_id>` 类型分支（每个有依赖的任务一条），以及 `agent/<task_id>` 类型分支。 |
| **前端事件验证** | 在执行过程中切到 **Workspace** 视图，每个有上游的任务在分配前能看到一条 toast / 日志条目（`task-base-prepared` 事件），含 `parentCount` 与 `conflictCount`。 |
| **通过?** | ☐ |

### M-14 下游 Agent 能看到上游产物

| 项 | 内容 |
|---|---|
| **操作** | 等到第二个（依赖第一个）的 task 进入 `running`，切到 Workspace → Focus 这个 agent，找到它系统 prompt 中的 `[Upstream Context]` 段（在 agent_events 的第 1 条 `system_prompt` 里）。 |
| **预期 ①** | `[Upstream Context]` 段列出了上游 task 的 `completion_summary` 与 `published artifacts`（如果 publish 了）。 |
| **预期 ②** | Agent 在第 1–3 步内调用 `read_file` 直接读上游产生的文件（如 `package.json` / `server.ts`），而不是从空目录新建。 |
| **通过?** | ☐ |

### M-15 ⚑ Codebase Intelligence 注入（FR-10）

| 项 | 内容 |
|---|---|
| **操作** | 用 1.5 准备的 `~/miragenty-fm15-test/rust-mini` 仓库 from_existing 创建 mission，需求："给 main 加一个命令行参数解析（用 std::env::args），把第一个参数作为 greeting 文案打印；附 README 一行说明。" Quick Plan → Confirm → Start。 |
| **预期 ①** | 任意 task 的 system_prompt（agent_events 第一条）必含 `[Project Structure]` 段（一棵 tree -L 3 风格的目录树，至少出现 `src/main.rs`、`Cargo.toml`）。 |
| **预期 ②** | 还必含 `[Tech Stack]` 段，至少识别出 `rust` + `cargo`。 |
| **预期 ③** | 如果该 task 有上游父任务且 prepare_task_base 出现冲突（一般小项目无），还会多出 `[Base Conflicts]` 段，按文件分组列出。 |
| **失败判定** | 缺少 `[Project Structure]` 或 `[Tech Stack]` → **判 M-15 不通过**。 |
| **通过?** | ☐ |

### M-16 Frontier Merge 顺序合入 main

| 项 | 内容 |
|---|---|
| **预期 ①（事件）** | 全部 task completed 后，前端会按拓扑序逐条收到 `mission-merge-progress`（每条带 `branch` 和 `status="merged"`），最后收到 1 条 `mission-merge-completed`。 |
| **预期 ②（git）** | `git -C <repo_path> log --oneline main`：能看到 N 条 `Merge agent/<task>` 类型的 commit；frontier task 数等于合并 commit 数（叶子节点合，中间被覆盖的不重复合）。 |
| **DB 验证** | `sqlite3 …miragenty.db "SELECT mission_id, branch, final_strategy FROM merge_records WHERE mission_id='<id>';"`：每条 frontier 都有一行，`final_strategy` 在 `"auto" / "heuristic_theirs" / "llm_resolve"` 之内。 |
| **通过?** | ☐ |

---

## 8. Phase 3：Guardrail / Codebase Intelligence / LLM 解冲突 / LlmJudge

### M-17 Agent 必须显式调用 task_complete 才算完成（FR-09.3）

| 项 | 内容 |
|---|---|
| **操作** | 任意 mission 的某一 task 完成后切 Workspace → Focus 该 agent，在 events 时间线里**全文搜索 `task_complete`**。 |
| **预期** | 每个 `completed` 状态的 task 对应的 agent 流里**必定能找到** `tool_use` 是 `task_complete` 的步骤，summary 字段非空。 |
| **失败判定** | task 显示 completed 但 agent 流里找不到 `task_complete`，或者 agent 输出大段总结性文本就被判完成 → **判 M-17 不通过**。 |
| **通过?** | ☐ |

### M-18 Guardrail 失败可注入重试提示

| 项 | 内容 |
|---|---|
| **操作** | 准备一个 mission，给某个 task 在 Planner 阶段编辑后**手动设置一个 produces_artifact**（例如 `api_spec`，类型 `api_spec`），description 写得让 Agent 不会真的产出该 artifact（例如只让它写 README）。Start。 |
| **预期 ①** | Agent 第一次 `task_complete` 后，系统自动注入一条 `[guardrail] artifact 'xxx' was not published — please publish it via publish_artifact and call task_complete again.` 的 user 消息（events 里能看到 `system_hint`/`tool_result` 含 guardrail 字样）。 |
| **预期 ②** | Agent 在重试预算（默认 3）耗尽前能补救则 task → completed；耗尽则 task → failed，且 `tasks.last_error` 写入形如 `guardrail: …` 的原因（M-29 会更详细看这个字段）。 |
| **通过?** | ☐ |

### M-19 ⚑ L3 LLM 解冲突走通（FR-08.2）

| 项 | 内容 |
|---|---|
| **操作** | 设计一个 fan-out → fan-in 的 4 节点 DAG：A → B、A → C、B → D、C → D；让 B 和 C **同时修改同一个文件的同一段**（例如都要在 `src/lib.rs` 的某行写不同 `pub fn` 签名）。在 Plan 完成后、Start 之前，从 DB 把该 mission 的 `merge_strategy` 改成 `'llm_resolve'`：`UPDATE missions SET merge_strategy='llm_resolve' WHERE id='<id>';`。然后回 Miragenty 启动。 |
| **预期 ①** | 后端日志里能看到 `LlmProviderResolver: resolving conflict` 或 `llm_merge` 相关行。 |
| **预期 ②** | `merge_records` 表里至少有一行 `final_strategy='llm_resolve'`，`llm_resolution_succeeded` 为 `1` 或 `0`（成功 / 失败都可观察）。 |
| **预期 ③** | 若 LLM 解出来：main 分支上能看到一条额外的 `Merge: LLM-resolved` commit；若失败：日志里有 fallback 到 theirs 的提示，**不能崩溃**。 |
| **失败判定** | 进程崩溃 / 看不到 `merge_records` 写入 / log 全无 LLM 解冲突痕迹 → **判 M-19 不通过**。 |
| **通过?** | ☐ |

### M-20 ⚑ Guardrail::LlmJudge 工作（FR-09.4）

| 项 | 内容 |
|---|---|
| **操作** | 在 Planner 阶段（或人工 SQL 写入）给某个 task 添加一个 LlmJudge guardrail，criteria 写得严格但可达成（例如 `"The README must mention installation steps."`）。SQL 示例：`UPDATE tasks SET guardrails='[{"kind":"LlmJudge","criteria":"The README must mention installation steps.","retries":3}]' WHERE id='<task_id>';`。Start 这个 task。 |
| **预期 ①** | Agent `task_complete` 后，能在 agent_events 看到 `LlmJudge: passed=true reason=...` 或 `passed=false reason=...` 的记录。 |
| **预期 ②** | 当 criteria 故意改成不可达成（例如 `"The output must be in French."` 但 task 实际是英文 README），重试预算耗尽后 task → failed，`tasks.last_error` 含 `guardrail` 关键字。 |
| **失败判定** | LlmJudge 永远 pass 或永远 fail / 没有 LLM 调用记录 → **判 M-20 不通过**。 |
| **通过?** | ☐ |

---

## 9. Phase 4：mission-delivered 交付面板 + Chat Agent

### M-21 mission-delivered 事件聚合 payload（FR-14.1）

| 项 | 内容 |
|---|---|
| **操作** | 完整跑通一个含 ≥ 2 个 published artifact 的 mission（例如 task A 产出 `design_doc`，task B 产出 `code_module`）。等到 mission 状态变 `completed`。 |
| **预期 ①** | mission 完成的瞬间，DAG 图上方出现 **Mission Delivered** 面板（橙色边的卡片，标题 "Mission Delivered"）。 |
| **预期 ②** | 该面板列出：repo path（可点击）、main branch、total tasks、total commits、artifacts 列表（含 local_name + artifact_type + 文件路径列表）。 |
| **预期 ③** | 浏览器 DevTools Console（dev 模式可见）：`window.__TAURI_INTERNALS__` 存在；前端 store 接收到 `mission-delivered` 事件，payload 同时包含：`missionId / repoPath / mainBranch / totalTasks / totalCommits / artifacts[].localName / llmResolvedFiles[] / autoResolvedFiles[]`。 |
| **通过?** | ☐ |

### M-22 Open in Editor / Terminal / Finder（FR-14.3）

| 项 | 内容 |
|---|---|
| **操作** | 在 Mission Delivered 面板上点击三个按钮各一次。 |
| **预期 ①** | "Open in Editor"：默认调系统 `open` 协议（macOS）/ `xdg-open`（Linux）/ `start`（Windows）打开 repo 目录；若机器上有 VS Code 命令 `code` 也能识别。 |
| **预期 ②** | "Open Terminal"：弹出新的终端窗口，cwd 就是 repo_path。 |
| **预期 ③** | "Reveal in Finder"（macOS） / Explorer（Win）/ Files（Linux）：在文件管理器中高亮该目录。 |
| **失败判定** | 任意按钮静默失败（无反应、无报错） → **M-22 不通过**。 |
| **通过?** | ☐ |

### M-23 ⚑ LLM-resolved / auto-merged 文件高亮提醒

| 项 | 内容 |
|---|---|
| **操作** | 用 M-19 同样的 fan-in 冲突 mission 跑通后查看交付面板。 |
| **预期 ①** | 当 mission 包含 LLM 解冲突文件时，交付面板显示橙色警告块 `⚠ N file(s) resolved by AI — please review`，列出每个文件路径。 |
| **预期 ②** | 当 mission 包含被 theirs/启发式自动解决的文件，显示另一块 `⚠ N file(s) auto-merged — verify if needed`。 |
| **失败判定** | 这两个提示块不显示 / 显示了但文件路径为空 → **判 M-23 不通过**。 |
| **通过?** | ☐ |

### M-24 Chat Agent 处理小改动（FR-15.5）

| 项 | 内容 |
|---|---|
| **操作** | 在已 completed 的 mission 上方滚动，找到 **Follow-up Chat** 面板。输入：`Add a comment "// hello FM-15" to the top of README.md`，⌘+Enter 发送。 |
| **预期 ①** | Chat 流里出现 user / assistant 气泡，assistant 区有流式 token 增量。 |
| **预期 ②** | Assistant bubble 标注 "task_complete"，并附 commit_hash / files_changed=1 / lines_changed≤2。 |
| **预期 ③** | 终端 `cd <repo_path> && git log --oneline -1` 能看到一条 `chat: ...` 类型的 commit。 |
| **预期 ④** | 没有触发 propose_followup_mission 弹窗（因为远低于 30 行硬阈值）。 |
| **通过?** | ☐ |

### M-25 ⚑ propose_followup_mission 流程闭环

| 项 | 内容 |
|---|---|
| **操作** | 在同一个 chat 里再发：`Refactor the entire crate into a workspace with frontend / backend / shared subcrates and add full CI`。等待 chat agent 返回。 |
| **预期 ①** | Chat 面板出现橙色提议卡片 `Escalate to a follow-up mission?`，显示 Title / Why / Estimated tasks。 |
| **预期 ②** | 点击 **"Yes, plan it as a new mission"**：后端创建子 mission（`SELECT id, parent_mission_id FROM missions WHERE parent_mission_id='<当前 mission id>'` 至少 1 行）；前端自动选中子 mission（左侧列表能看到新条目，状态 `draft`）；在子 mission 上手动跑 Plan → 出现完整 DAG。 |
| **预期 ③** | 点击 **"No, just do it directly"**：chat 面板出现一条 system 消息 `[rejected] User declined escalation.`；顶部出现 `direct mode` 徽标；再发同一句指令 → chat agent 不再调 propose；要么完成（≤30 行）要么 commit_failed/`rejected_oversize`（>30 行被守门员拒绝）。 |
| **失败判定** | 弹窗不出现 / 点了"Yes"后没有创建子 mission / 点了"No"后还是弹同样窗 → **判 M-25 不通过**。 |
| **通过?** | ☐ |

---

## 10. Follow-up：多层超时看门狗 + 一键重启 + 失败可视化 + shell 实时流

> Follow-up 修复了"agent 总是 failed + 重启体验差"的痛点。本节验证四件事：
> 1. 多层超时（L1 stream-idle / L2 shell idle+wall / L3 read-only loop / L4 wall-clock）能各司其职。
> 2. 失败原因被持久化到 `tasks.last_error` 并在 DAG / TaskDetailPanel 可见。
> 3. 一键重启（`auto_start=true`）省掉重新选目录、点开始等操作。
> 4. shell_exec 的 stdout / stderr 实时流式 emit 到 Workspace 视图。

### M-26 LLM stream-idle 兜底（L1）

> 模拟 LLM 卡住：把 Settings → `LLM Stream Idle Timeout` 改成 **5 秒**（最低值）→ Save。然后切到一个能联网但不稳定的环境（或临时关 Wi-Fi 几秒制造网络抖动）。

| 项 | 内容 |
|---|---|
| **操作（推荐）** | Settings → Stream Idle Timeout 改成 `5` → Save。任意建一个新 mission，Quick Plan，**在 PlannerStreamPanel 出现首个 token 后立刻关闭网络 6 秒，再开**。 |
| **预期 ①** | 不会无限卡住；最多 5 秒后看到一次错误（`stream_idle_timeout` 或 `Provider stream idle for >5s`）落在 PlannerStreamPanel / 后端日志。 |
| **预期 ②** | 如果之前已经收到过部分内容，错误处理会把"已收到的部分文本"作为最终 response 返回，不抛 panic。日志关键字：`partial content` 或 `idle timeout, returning partial`。 |
| **回滚** | 测完把 Stream Idle 改回 `60`。 |
| **通过?** | ☐ |

### M-27 ⚑ shell_exec watchdog（L2）

| 项 | 内容 |
|---|---|
| **操作 ①（idle 触发）** | 准备一个 mission，让某个 task 的 description 让 agent 倾向跑 `sleep 200`（例如要求 "verify by waiting 3 minutes"）。Start。或者更可控：用 `cargo test --lib executor::tests::shell_idle` 这类测试自动化覆盖（已经在 M-01 覆盖）。 |
| **预期 ①** | shell_exec 在默认 60s idle 后被 SIGKILL；返回的 ToolOutput.content 是 JSON，含 `"error":"shell_killed"` / `"reason":"idle 60s ..."` / `"hint": ...`。后端日志一行 `shell_exec watchdog terminating: ...`。 |
| **操作 ②（wall-clock 触发）** | 让 agent 跑 `yes > /dev/null`（一直有输出但不停）。 |
| **预期 ②** | 默认 5 分钟（`SHELL_DEFAULT_WALL_SECS=300`）后被 kill，错误结构同 ①，`reason` 形如 `"wall_clock 300s exceeded ..."`。 |
| **操作 ③（expect_long_running）** | Agent 显式 `expect_long_running: true` 跑 `pnpm install`：阈值升到 120s idle / 30min wall，正常完成不被误杀。 |
| **预期 ③** | 命令在合理时间内成功完成；agent_events 看不到 watchdog kill 字样。 |
| **失败判定** | 任一情形导致 agent 主进程被卡死、Miragenty 整体无响应 → **判 M-27 不通过**。 |
| **通过?** | ☐ |

### M-28 read-only loop 检测（L3）

| 项 | 内容 |
|---|---|
| **操作** | 给某个 task description 写得非常模糊（例如 "explore the codebase deeply and tell me what you find"），让 agent 反复 list_files / read_file / search_files。 |
| **预期 ①** | 当连续 ≥ 5 步 全部是只读工具时，agent_events 出现一条 `system_hint`，内容形如 `[System] You have spent 5 consecutive steps only reading / searching files ...`，提醒 agent 转入产出。 |
| **预期 ②** | hint 只注入一次，不会每步重复刷屏。 |
| **通过?** | ☐ |

### M-29 失败原因落库 + UI 可视化

| 项 | 内容 |
|---|---|
| **操作** | 用 M-27 / M-20 等故意触发的失败任务作为对象。 |
| **DB 验证** | `sqlite3 …miragenty.db "SELECT id, status, last_error, last_failed_at FROM tasks WHERE status='failed' ORDER BY last_failed_at DESC LIMIT 3;"`：每条 `last_error` 非空，前缀含 `timeout:` / `guardrail:` / `worktree_error:` / `cancelled:` / `max_steps:` 之一；`last_failed_at` 是 ISO 时间戳。 |
| **UI 验证 ①（DAG 节点 tooltip）** | 在 DAG 上 hover 一个 failed 节点，tooltip 内出现红色块"Last error"，含 last_error 文本和时间。 |
| **UI 验证 ②（TaskDetailPanel）** | 选中一个 failed 节点，右侧 TaskDetailPanel 出现红色字背景的错误段（`pre` 块），完整显示 `last_error`，下方一行小字显示 `Failed at: <time>`。 |
| **通过?** | ☐ |

### M-30 一键重启（auto_start）

| 项 | 内容 |
|---|---|
| **操作** | 任选一个状态为 `failed` 的 mission，点击右上角 **Restart** → 选 "Failed only" 或 "Full"，点 Confirm。 |
| **预期 ①** | **不再弹出选目录的 Repository Picker**（因为 `restart_mission` 复用了原 `repo_path`）。 |
| **预期 ②** | mission 状态在 1–2 秒内变成 `running`；之前 failed 的 task 自动变 `pending` 或 `ready`，DAG 上 last_error 消失。 |
| **预期 ③** | 若由于 repo_path 不再存在或权限问题导致 auto_start 失败，前端会回退到旧弹窗（让用户重新选目录），不会静默失败。 |
| **DB 验证** | `SELECT status, last_error, last_failed_at FROM tasks WHERE mission_id='<id>';`：原 failed 任务 last_error / last_failed_at 已被清空（NULL）。 |
| **通过?** | ☐ |

### M-31 ⚑ shell_exec 实时流（agent-tool-stream）

| 项 | 内容 |
|---|---|
| **操作** | 任选一个会跑 shell_exec 的 task（M-15 / M-27 期间任意 mission 均可），切到 Workspace → Focus 该 agent。 |
| **预期 ①** | 在 agent 调用 shell_exec 期间，AgentTerminalPane 下方出现一个**蓝色描边的 "shell" 块**，实时滚动显示当前命令的输出（包含 `$ <command>` 起手 meta、stdout 文本、`[stderr] ...`、`[watchdog kill] ...` 等）。 |
| **预期 ②** | 命令结束（或被 kill）后该块仍保留可滚动；agent 进入下一个 step / 状态变 terminal 时清空。 |
| **预期 ③** | DevTools Console / 后端日志能看到 `agent-tool-stream` 事件，payload 形如 `{ agent_id, tool: "shell_exec", stream: "stdout"|"stderr"|"meta", chunk, eof }`。 |
| **失败判定** | 整个 mission 跑完 Workspace 都看不到 shell 块，或者命令完成后 shell 块永远不消失反复堆积 → **判 M-31 不通过**。 |
| **通过?** | ☐ |

---

## 11. 全链路自动化回归

### M-32 全量后端单测 + 前端构建

```bash
cd src-tauri && cargo test --lib --quiet
cd .. && pnpm tsc --noEmit && pnpm test --run && pnpm build
```

**预期**：四条命令全 exit 0；后端 `294 passed; 0 failed`（数字可能随后续提交略变）；`pnpm test --run` 全部通过。

| 通过? | ☐ |

---

## 12. 验收结果汇总

把每条 M-XX 的勾选状态填到下表，全部 ✅ 即 FM-15 验收通过：

| 编号 | 阶段 | 标题 | 强制? | 通过? |
|---|---|---|:-:|:-:|
| M-01 | — | 自动化测试基线 | **⚑** | ☐ |
| M-02 | P1-A | Quick Plan：Mission 创建并落库 | — | ☐ |
| M-03 | P1-A | Planner Agent Loop 透传 | — | ☐ |
| M-04 | P1-A | DAG 富语义渲染 | **⚑** | ☐ |
| M-05 | P1-A | 状态流转 draft→planned | — | ☐ |
| M-06 | P1-B | Mission Contract 落库 | — | ☐ |
| M-07 | P1-B | Pre-flight 路径走相同 PlannerEngine | **⚑** | ☐ |
| M-08 | P1-B | Planner 真的探索了代码 | — | ☐ |
| M-09 | P1-B | Contract 富语义穿透 DAG | **⚑** | ☐ |
| M-10 | P1-B | Scope/Exclusions Guardrail 信号 | — | ☐ |
| M-11 | P1-C | fetch_url 用户确认 | — | ☐ |
| M-12 | P1 | 数据完整性 & 无回归 | **⚑** | ☐ |
| M-13 | P2 | 增量 Worktree 落库 + 事件透传 | — | ☐ |
| M-14 | P2 | 下游 Agent 能看到上游产物 | — | ☐ |
| M-15 | P2/P3 | Codebase Intelligence 注入 | **⚑** | ☐ |
| M-16 | P2 | Frontier Merge 顺序合入 main | — | ☐ |
| M-17 | P3 | Agent 必须显式 task_complete | — | ☐ |
| M-18 | P3 | Guardrail 失败可注入重试提示 | — | ☐ |
| M-19 | P3 | L3 LLM 解冲突走通 | **⚑** | ☐ |
| M-20 | P3 | Guardrail::LlmJudge 工作 | **⚑** | ☐ |
| M-21 | P4 | mission-delivered 事件聚合 payload | — | ☐ |
| M-22 | P4 | Open in Editor / Terminal / Finder | — | ☐ |
| M-23 | P4 | LLM-resolved / auto-merged 文件高亮提醒 | **⚑** | ☐ |
| M-24 | P4 | Chat Agent 处理小改动 | — | ☐ |
| M-25 | P4 | propose_followup_mission 流程闭环 | **⚑** | ☐ |
| M-26 | F | LLM stream-idle 兜底（L1） | — | ☐ |
| M-27 | F | shell_exec watchdog（L2） | **⚑** | ☐ |
| M-28 | F | read-only loop 检测（L3） | — | ☐ |
| M-29 | F | 失败原因落库 + UI 可视化 | — | ☐ |
| M-30 | F | 一键重启（auto_start） | — | ☐ |
| M-31 | F | shell_exec 实时流（agent-tool-stream） | **⚑** | ☐ |
| M-32 | — | 全量后端单测 + 前端构建 | **⚑** | ☐ |

> 阶段缩写：P1-A = Phase 1 Quick Plan、P1-B = Phase 1 Pre-flight、P1-C = Phase 1 fetch_url、P2 = Phase 2、P3 = Phase 3、P4 = Phase 4、F = Follow-up。

---

## 13. 已知限制 / 超出 FM-15 范围（不要据此扣分）

为避免误判，下列现象 **不属于 FM-15 验收范围**：

1. **mission 必须有联网 LLM**：所有 Planner / Agent / Chat 调用都依赖 LLM，本验收要求测试机能稳定访问配置的 base_url。如果测试时网络断断续续，部分用例（特别是 M-26 之外的）会误判失败——请先确保网络稳定。
2. **`fetch_url` 默认白名单为空**：意味着每次都需要手动确认，这是默认安全行为。如果用户在 Settings 里加了 `planner_fetch_allowlist`，则该域可免确认。
3. **Chat Agent 直接 commit 的硬阈值是 30 行**：`commit_main_workdir` 工具内置守门员，超过 30 行直接 reject 强制走 propose 路径。这是有意的安全阀门。
4. **正在运行的 Agent 不会切到新的超时配置**：Settings 修改 `agent_step_idle_seconds` / `agent_timeout_seconds` 后，**新启动的** Agent / Chat / Planner 会立即生效；**正在跑** 的 Agent 直到结束都用旧值。这是 LLM Provider 一次性构造的设计。
5. **shell_exec 输出限长 16KB（尾部）**：长跑命令的早期输出会被丢弃，只保留末尾，避免内存爆炸。这是一个有意的折中。
6. **`mission_chats` / `task_base_conflicts` / `merge_records` 表 不会自动清理**：长期运行下数据持续增长。后续会引入归档策略，本期不做。

---

## 14. 故障排查（Troubleshooting）

| 现象 | 处置 |
|---|---|
| `pnpm tauri dev` 卡在 `Compiling` 超过 5 分钟 | 是首次编译 Rust 依赖，正常。如果 10 分钟无进展，停掉重跑，并检查 `~/.cargo` 写权限。 |
| 启动后窗口一直白屏 | 终端里看错误。常见是端口 1420 被占；改用 `VITE_PORT=1421 pnpm tauri dev` 测试。 |
| 点 Quick Plan 后 PlannerLoopPanel 一直空白 | ① 检查 LLM Key 是否对了。② 在 Settings 把 `planner_timeout_seconds` 调大到 1200。③ 看终端是否有 LLM 相关 401/429 错误。 |
| Pre-flight 没法 Sign（按钮灰着） | Contract 面板要求 Scope 至少 1 条；多跟 AI 对话两轮就行。 |
| DAG 上看不到 Role/Artifact 徽章 | 确认 M-01 通过；如果通过仍看不到，按 M-04 的 DB 验证步骤跑 SQL，如果 DB 字段是空的说明 Planner 阶段就没写对——这是真问题，请截图 + 数据库导出（`.dump tasks` 与 `.dump task_dependencies`）反馈。 |
| 任何 task 长时间 `running` 不动 | ① 看 Workspace 上该 agent 的 shell 块是否还在更新——若 stdout 还在打，等它跑完。② 若 shell 块停止但 agent 仍 running 超过 30min，是 wall-clock 兜底应触发但未触发，这是真 bug。③ 改 Settings 里的 `agent_timeout_seconds` 临时降到 600 验证一下兜底逻辑。 |
| Restart 后还是失败 | 看 `tasks.last_error`。若是 `worktree_error: ...` 类原因，说明仓库目录被外部改坏，删掉 `<repo_path>/.git/worktrees/` 后再 Restart。 |
| `sqlite3` 提示 `database is locked` | Miragenty 在跑时持锁。先关 Miragenty 再查，或者改用 `sqlite3 -readonly <file>`。 |
| Workspace 上看不到 shell 流 | ① 当前 task 没跑 shell 命令，看 events 是否有 `tool_use shell_exec`。② DevTools Console 看 `agent-tool-stream` 事件是否有到达。③ 若都没有，是事件桥接 bug，反馈。 |

---

## 15. 反馈模板

如发现任一阻断项（**⚑**）不通过：

```
- 失败项：M-XX
- 操作复现路径：1. 我点了…  2. 我看到…  3. 期望是…
- 终端关键日志（贴最后 50 行）：
- 数据库导出：
  sqlite3 …miragenty.db ".dump missions"           > /tmp/missions.sql
  sqlite3 …miragenty.db ".dump tasks"              > /tmp/tasks.sql
  sqlite3 …miragenty.db ".dump task_dependencies"  > /tmp/deps.sql
  sqlite3 …miragenty.db ".dump planner_steps"      > /tmp/planner_steps.sql
  sqlite3 …miragenty.db ".dump merge_records"      > /tmp/merge_records.sql
  sqlite3 …miragenty.db ".dump task_base_conflicts"> /tmp/task_base_conflicts.sql
- 屏幕截图（DAG 视图 / Tooltip / PlannerLoopPanel / MissionDeliveryPanel / Workspace shell 块）
```

把以上信息附在 issue / 工单里即可。

---

## 16. 快速回归清单（30 分钟版）

> 适用于 PR review 等只需快速验证「主链路没碎」的场景。挑下表所列的最小子集跑一遍即可。

| 编号 | 题目 | 预计耗时 |
|---|---|---|
| M-01 | 自动化基线 | 1 min |
| M-02 + M-04 | Quick Plan + DAG 富语义渲染 | 5 min |
| M-13 + M-14 | 增量 Worktree + Upstream Context | 8 min |
| M-17 + M-29 | task_complete + last_error 可视化 | 5 min |
| M-21 + M-22 | mission-delivered + Open in ... | 5 min |
| M-30 + M-31 | 一键重启 + shell 实时流 | 4 min |
| M-32 | 全量回归 | 2 min |

通过这 7 项即可初判「FM-15 主链路未退化」。完整回归仍以第 12 节汇总表为准。
