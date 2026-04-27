# FM-15 Phase 1 验收手册（Agent Orchestration Enhancement）

> 适用范围：FM-15 v2.2 Phase 1 — 数据模型 + Planner Agent Loop + Pre-flight 整合
>
> 阅读对象：**没有项目背景的测试人员**。本文不假设你了解 Tauri、Rust、React 或 Miragenty 历史架构；按章节顺序操作即可。
>
> 完成时长：建议预留 **60–90 分钟**（含一次性环境配置）。
>
> 通过门槛：**M-01 到 M-12 全部勾选 ✅**。其中 M-04、M-07、M-09、M-12 任一失败即记 Phase 1 不通过。

---

## 0. 名词速查（先看 1 分钟）

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
| **Worktree** | Phase 2 才启用。Phase 1 验收**不**涉及 Agent 实际执行任务。 |

---

## 1. 环境准备（一次性，做一次就好）

### 1.1 系统要求

- macOS 12+ / Windows 10+ / Linux x64
- 已安装：
  - Node.js ≥ 20，`pnpm` ≥ 9（运行 `pnpm -v` 应能显示版本号）
  - Rust toolchain ≥ 1.78（运行 `cargo --version`）
  - Git ≥ 2.30（运行 `git --version`）
  - SQLite 命令行工具 `sqlite3`（macOS 自带；Linux `apt install sqlite3`；Windows 装 [precompiled binary](https://www.sqlite.org/download.html)）
- 一个能联网的环境（首次启动需要拉依赖、Planner 调用 LLM）

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
6. 提示 "Saved" 后回到 **Missions** 页面。

> 如果你的 Key 是 OpenAI、Claude、DeepSeek 等其他厂商，请同步改 Base URL 和模型名。验收脚本本身与具体 Provider 无关。

### 1.5 准备两个测试目录

打开**新的**终端（不要关闭 1.3 的窗口），执行：

```bash
# 用作"已有仓库"测试目标
mkdir -p ~/miragenty-fm15-test/existing-repo
cd ~/miragenty-fm15-test/existing-repo
git init -q
mkdir -p src/auth src/legacy
echo 'export function login() { return "TODO"; }' > src/auth/login.ts
echo 'export function legacyPay() { return "DONT_TOUCH"; }' > src/legacy/payment.ts
echo '# Existing project' > README.md
git add . && git commit -q -m "init"

# from_scratch 路径无需手动准备，Miragenty 会自动在 ~/miragenty-workspaces/<slug>/ 下建目录
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
| **预期** | 四条命令依次输出："**236 passed; 0 failed**"、TypeScript 无任何输出（即 0 错）、"**Tests 25 passed (25)**"、"**built in <2s**"。 |
| **若失败** | Phase 1 不通过——开发同学先修 baseline 再继续。 |
| **通过?** | ☐ |

---

## 3. 路径 A：Quick Plan（直接 Planner，from_scratch）

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
- 下方 **Planner Agent Loop** 面板（标题就是这个字面值）：以**步骤卡片**形式展示，每步是一个圆角块，左上角带步号、工具名（如 `propose_task` / `add_dependency` / `validate_plan` / `finalize_plan`），右侧是耗时和 token 数。

整个过程通常持续 **20–90 秒**，结束后 Mission 状态自动变为 `draft`，DAG 出现在主画布。

### M-03 Planner Agent Loop 透传

| 项 | 内容 |
|---|---|
| **预期 ①** | 下方 **Planner Agent Loop** 面板在点击 Quick Plan 后 **3 秒内** 出现至少 1 个步骤卡片（不是空白等好几十秒）。 |
| **预期 ②** | 看到的工具名至少包括 `propose_task`、`add_dependency`、`validate_plan`、`finalize_plan` 这几种（可能还有 `list_skills` / `read_file` / `list_directory`，正常）。 |
| **预期 ③** | 终端日志（1.3 那个窗口）出现 `planner_engine` 相关行，最后一行类似 `complete_planner_session` 且 `total_steps` > 0、`total_tokens` > 0。 |
| **通过?** | ☐ |

### M-04 DAG 富语义渲染（核心验收）

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

> ⚠️ **此处不要点 "Start"**。Phase 2 才启用 Agent 真正执行；Phase 1 验收只到 `planned` 状态为止。

---

## 4. 路径 B：Pre-flight + Planner（from_existing）

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

### M-07 Pre-flight 路径的 Planner 也跑 Agent Loop（核心验收）

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

### M-09 Contract 富语义穿透 DAG（核心验收）

| 项 | 内容 |
|---|---|
| **预期 ①（DAG 节点 Role 多样化）** | 至少出现 2 种 Role（密码重置类需求，典型 architect → implementer → tester 三角色组合）。 |
| **预期 ②（边带 ArtifactBadge）** | 至少有 **1 条依赖边** 上出现蓝色 ArtifactBadge（小圆点 + 数字 ≥1），hover 显示 `Artifacts: <local_name>`，且这个 local_name 形如 `api_spec`、`design_doc`、`test_module` 等。 |
| **预期 ③（task tooltip 显示 Consumes）** | 被 Badge 指向的下游 task，hover 展开 tooltip 后能看到 `Consumes:` 区块，里面有该 artifact local_name。 |
| **DB 验证** | `SELECT task_id, depends_on, artifact_refs FROM task_dependencies WHERE task_id IN (SELECT id FROM tasks WHERE mission_id='<id>');`：至少 1 行 `artifact_refs` 不是 `[]`，且其内容是 `["<uuid>.<local_name>"]` 形式。 |
| **通过?** | ☐ |

### M-10 Scope/Exclusions Guardrail 信号

> 这是 S4 加的轻量 Guardrail。它**不阻止** finalize，但应该在 `validate_plan` 工具调用结果里以 warn 形式出现。

| 项 | 内容 |
|---|---|
| **DB 验证（scope 覆盖）** | `sqlite3 …miragenty.db "SELECT tool_name, tool_result FROM planner_steps WHERE session_id=(SELECT id FROM planner_sessions WHERE mission_id='<id>' ORDER BY created_at DESC LIMIT 1) AND tool_name='validate_plan';"`：返回 ≥ 1 行。如果 LLM 一次就把每条 scope 覆盖好，`tool_result` 中可能没有 warn——这是正面情况。如果有 warn，应当看到形如 `"code":"WARN_SCOPE_NOT_COVERED"` 或 `"code":"WARN_EXCLUSION_TOUCHED"` 的字段，且 `"severity":"warn"`。 |
| **故意触发实验（可选，10 分钟）** | 1) 重新建一个 from_existing Mission，需求写成"重写 src/legacy 模块"。2) Pre-flight 时，故意在 Exclusions 里加一条"不要修改 src/legacy"。3) Sign 后观察 PlannerLoopPanel 中 `validate_plan` 的 result。**预期**：要么 LLM 自己改任务路径避开冲突，要么至少出现一次 `WARN_EXCLUSION_TOUCHED`。无论哪一种，最终 finalize 仍能成功（warn 不阻塞）。 |
| **通过?** | ☐ |

---

## 5. 路径 C：fetch_url 工具的用户确认（独立小验收）

> 这一节验证 Planner 调用 `fetch_url` 工具时，前端弹窗能正确显示并按用户决策放行 / 拦截。本节**最容易在没有外网时跳过**，不勾选不影响 Phase 1 通过，但若有外网建议跑一遍。

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

## 6. 收尾验收

### M-12 数据完整性 & 无回归

| 项 | 内容 |
|---|---|
| **预期 ①（无报错）** | 退出 Miragenty（关窗口），回看 1.3 终端：搜索 `ERROR ` 关键字（注意大小写）。**预期**：除上面故意触发的 fetch deny / host blocked 之外，没有任何未捕获 ERROR；尤其不应有 `panicked at`、`thread '...' panicked` 字样。 |
| **预期 ②（DB schema）** | `sqlite3 …miragenty.db ".tables"` 应包含：`missions`, `tasks`, `task_dependencies`, `mission_contracts`, `contract_items`, `preflight_sessions`, `planner_sessions`, `planner_steps`, `planner_session_fetch_grants`, `artifacts`, `task_base_conflicts`, `merge_records`。一张都不能少。 |
| **预期 ③（schema 字段）** | `sqlite3 …miragenty.db "PRAGMA table_info(tasks);"` 应包含字段 `role`, `expected_output`, `additional_skills`, `produces_artifacts`, `consumes_artifacts`, `file_scope_hints`, `guardrails`, `actual_files_modified`。`PRAGMA table_info(task_dependencies);` 应包含 `artifact_refs`。 |
| **预期 ④（重启不丢数据）** | 关 Miragenty 再 `pnpm tauri dev` 启动一次：之前创建的 missions 全部出现在左侧列表，点开 DAG 仍能看到 RoleBadge / ArtifactBadge。 |
| **通过?** | ☐ |

---

## 7. 验收结果汇总

把每条 M-XX 的勾选状态填到下表，全部 ✅ 即 Phase 1 通过：

| 编号 | 标题 | 强制? | 通过? |
|---|---|:-:|:-:|
| M-01 | 自动化测试基线 | 是 | ☐ |
| M-02 | Mission 创建并落库 | — | ☐ |
| M-03 | Planner Agent Loop 透传 | — | ☐ |
| M-04 | DAG 富语义渲染 | **是** | ☐ |
| M-05 | 状态流转 draft→planned | — | ☐ |
| M-06 | Mission Contract 落库 | — | ☐ |
| M-07 | Pre-flight 路径走相同 PlannerEngine | **是** | ☐ |
| M-08 | Planner 真的探索了代码 | — | ☐ |
| M-09 | Contract 富语义穿透 DAG（边带 Artifact） | **是** | ☐ |
| M-10 | Scope/Exclusions Guardrail 信号 | — | ☐ |
| M-11 | fetch_url 用户确认 | — | ☐ |
| M-12 | 数据完整性 & 无回归 | **是** | ☐ |

---

## 8. 已知与 Phase 2/3 才会做的事项（不要据此扣分）

为避免误判，下列现象 **不属于 Phase 1 验收范围**：

1. **不会真的开始执行 Task**：Phase 1 只到 `Confirm` 后变 `planned`。点 `Start` 启动 Agent 是 Phase 2 的事。
2. **不会创建 worktree、不会自动 merge**：Worktree 增量工作区是 Phase 2。
3. **Guardrail 不强制**：FR-21.3 文档里写的"LLM-Judge + 硬阻塞 finalize"，Phase 1 实现是 **启发式 + warn-only**——LLM 看到 warn 后是否修复全凭它自己。这是有意识的折中，不算缺陷。
4. **Agent 完成判定**：Phase 3 才会用 `task_complete` + Guardrail 校验，Phase 1 不涉及。
5. **冲突解决（含 LLM 解冲突）、Codebase Intel 探针、终态交付面板、Follow-up Chat**：分别是 Phase 2 / Phase 3 / Phase 4 范围。
6. **`fetch_url` 默认白名单为空**：意味着每次都需要手动确认，这是默认安全行为。如果用户在 Settings 里加了 `planner_fetch_allowlist`，则该域可免确认。

---

## 9. 故障排查（Troubleshooting）

| 现象 | 处置 |
|---|---|
| `pnpm tauri dev` 卡在 `Compiling` 超过 5 分钟 | 是首次编译 Rust 依赖，正常。如果 10 分钟无进展，停掉重跑，并检查 `~/.cargo` 写权限。 |
| 启动后窗口一直白屏 | 终端里看错误。常见是 LLM Key 没配（这种情况页面只是没数据，并非白屏；白屏一般是端口 1420 被占）。 |
| 点 Quick Plan 后 PlannerLoopPanel 一直空白 | ① 检查 LLM Key 是否对了。② 在 Settings 把 `planner_timeout_seconds` 调大到 1200。③ 看终端是否有 LLM 相关 401/429 错误。 |
| Pre-flight 没法 Sign（按钮灰着） | Contract 面板要求 Scope 至少 1 条；多跟 AI 对话两轮就行。 |
| DAG 上看不到 Role/Artifact 徽章 | 确认 M-01 通过；如果通过仍看不到，按 M-04 的 DB 验证步骤跑 SQL，如果 DB 字段是空的说明 Planner 阶段就没写对——这是真问题，请截图 + 数据库导出（`.dump tasks` 与 `.dump task_dependencies`）反馈。 |
| `sqlite3` 提示 database is locked | Miragenty 在跑时持锁。先关 Miragenty 再查。 |

---

## 10. 反馈模板

如发现 M-04 / M-07 / M-09 / M-12 中任一项不通过：

```
- 失败项：M-XX
- 操作复现路径：1. 我点了…  2. 我看到…  3. 期望是…
- 终端关键日志（贴最后 30 行）：
- 数据库导出：
  sqlite3 …miragenty.db ".dump missions" > /tmp/missions.sql
  sqlite3 …miragenty.db ".dump tasks" > /tmp/tasks.sql
  sqlite3 …miragenty.db ".dump task_dependencies" > /tmp/deps.sql
  sqlite3 …miragenty.db ".dump planner_steps" > /tmp/planner_steps.sql
- 屏幕截图（DAG 视图 / Tooltip / PlannerLoopPanel）
```

把以上信息附在 issue / 工单里即可。
