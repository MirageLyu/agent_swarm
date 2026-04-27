# FM-15: Agent Orchestration Enhancement

> 版本: v2.2 | 日期: 2026-04-13  
> 优先级: P0 | 预估周期: 3-4 周（分 4 个 Phase 实施）  
> 依赖: FM-01（Mission Planning）、FM-02（Multi-Agent Orchestration）、FM-10（Pre-flight Contract） | 被依赖: FM-12, FM-13  
> 定位: 将 DAG 从「**调度图**」升级为「**协作图**」——节点带角色 / 技能 / 验收契约，边带产物流，运行时通过**增量 worktree** 让下游 Agent 真正在上游产出之上工作。Planner 升级为**探索式 Agent Loop**（按需 grounding，而非预先塞全量上下文）。Pre-flight 与 Planner 通过 Mission Contract 串联，Pre-flight 在已有 repo 模式下也获得只读探索能力。同步替换脆弱的"无 tool_use 即完成"判定，引入 guardrail 完成检测、三层冲突合并（含 LLM 解冲突）、终态交付面板与 follow-up chat。

> **v2.1 变更**: Planner 从「单次 LLM 调用」改为「Agent Loop」(C-06 / FR-04 / FR-05 / FR-06)；新增 `fetch_url` 工具与白名单确认机制 (FR-05.6)；Planner 探索步骤持久化与流式展示 (FR-17 / 数据需求)。

> **v2.2 变更**: 整合 Pre-flight 与 Planner (C-07)；Mission 创建强制选择 `repo_origin`（from-scratch / from-existing）(FR-18)；Pre-flight 在已有 repo 模式下注入只读探索工具 (FR-19)；Contract 在 Planner Loop 内的双层注入（核心字段进 prompt + 详情按需查）(FR-20)；Scope Coverage / Exclusions Untouched 强制 guardrail (FR-21)；`sign_contract` 重构为薄壳，统一调用 `plan_mission(contract_id?)` (FR-22)。

---

## IR — Initial Requirements

### IR-01: 用户故事

**US-01**：作为开发者，我希望 Planner 生成的每个 Task 不仅有 description，还有**预期输出（验收契约）、负责角色、所需技能、文件影响范围**，这样下游环节有充分的判定依据。

**US-02**：作为开发者，我希望 DAG 的**边**不仅表达执行顺序，还显式声明**上游产出的 artifact**沿这条边流向下游，并能在 UI 边上看到 artifact 摘要。

**US-03**：作为开发者，我希望系统提供一组**预定义角色**（Architect、Implementer、Tester 等），每个角色有默认的 prompt、工具集、技能与验收方式，不同角色在 DAG UI 上有清晰的视觉区分。

**US-04**：作为开发者，我希望 Miragenty 兼容 **Cursor / Claude Code / Codex 的 SKILL.md 开放标准**，我已有的 skill 包零迁移可用。

**US-05**：作为开发者，我希望下游 Task 启动时，工作区已经包含上游所有 Task 的产出（**增量 worktree**），而不是从空 main 起步——这样多 Agent 协作才有意义。

**US-06**：作为开发者，我希望兄弟 Task 合并冲突时系统能**智能解决**（先尝试 git auto-merge，再降级到启发式策略，必要时调用 LLM 在完整上下文下解冲突），而不是简单地"后写覆盖前写"。

**US-07**：作为开发者，我希望 Agent 完成判定基于**guardrail 校验**（artifact 是否产出、build / test 是否通过等），而不是"是否输出了 tool_use"这种脆弱信号。

**US-08**：作为开发者，我希望 Mission 完成后看到一个**交付面板**：明确的产物目录、按 artifact type 分组的产出列表、diff 摘要、一键打开 workspace。

**US-09**：作为开发者，我希望在交付面板有一个**chat 框**——小修改 chat agent 直接做，大需求 agent 提议升级为 follow-up mini-mission，由我确认后走完整 plan→DAG→执行流程。

**US-10**：作为开发者，我希望 Mission 执行**全程无人值守**——一旦确认就不再被打断，任何模糊都通过 guardrail 重试或最终失败处理。

### IR-02: 业务价值

| 维度 | 现状 | 目标 |
|------|------|------|
| **DAG 表达力** | 节点只有 title/description/complexity，边只表示执行顺序 | 节点带 role/skills/artifacts/file_hints，边带 artifact_refs |
| **Agent 协作性** | 每个 worktree 从空 main 起步，互不可见 | 增量 worktree，下游天然看到上游产出 |
| **完成可靠性** | 模型偶尔输出"让我先想想"就被判定完成 | guardrail 校验（artifact/build/test）+ 重试预算 |
| **冲突处理** | 一律 `-X theirs`，下游覆盖上游 | 三层降级：自动→启发→LLM 解冲突 |
| **角色专业化** | 所有 Agent 共享同一份 prompt 和工具 | 6 种预定义角色，可装载 SKILL.md 技能包 |
| **产物可发现** | 用户必须自己 diff 找产物 | 交付面板按 artifact type 列出，一键打开 |
| **后续追加** | Mission 完成即终止，新需求要重启 mission | follow-up chat：小改即做，大改自动提议 mini-mission |

### IR-03: 高层验收标准

1. Planner 生成的 DAG 中，每个 Task 包含 `role / additional_skills / expected_output / file_scope_hints / produces_artifacts / consumes_artifacts` 字段
2. 用户可往 `~/.cursor/skills/`、`~/.claude/skills/`、`~/.miragenty/skills/` 任一目录放置 SKILL.md，Miragenty 自动发现并供 Planner 选用
3. DAG 可视化中，节点显示 role 标识（颜色/图标），边上显示 artifact 摘要 tooltip
4. 下游 Task 启动时，worktree 已合入所有直接父 Task 的分支
5. 兄弟 Task 修改同一文件时，三层合并策略按降级顺序工作；LLM 解冲突结果通过 build 校验，失败则回退到启发式
6. Agent 完成必须通过 guardrail 校验；失败则注入错误信息重试，重试预算耗尽 → task failed
7. Agent 执行有时间和步数双限制，超限优雅终止并保留已有产物
8. Mission 完成后弹出交付面板，包含产物列表 + diff 摘要 + 打开 workspace 按钮 + chat 输入框
9. Chat 中小改动直接执行；agent 判断为大改时弹窗提议 follow-up mission，用户确认后走完整流程
10. 单任务、线性、菱形、扇出/扇入四种拓扑均可端到端跑通（含至少一个 LLM 解冲突场景）

---

## 核心概念

### C-01: 数据模型全景

```text
Mission
  ├─ tasks: Task[]
  ├─ artifacts: Artifact[]      ← 全 mission 共享的产物池
  ├─ chat_messages: ChatMessage[]
  └─ followups: Mission[]       ← 子 mission（同表，parent_mission_id 关联）

Task
  ├─ id, title, description
  ├─ expected_output             ← 验收契约（自然语言）
  ├─ role                        ← architect | implementer | refactorer | tester | integrator | researcher
  ├─ additional_skills: SkillId[] ← 在 role 默认 skills 之上额外注入
  ├─ file_scope_hints
  │    ├─ definite: Glob[]       ← 高置信度
  │    └─ possible: Glob[]       ← 低置信度
  ├─ consumes_artifacts: ArtifactId[]
  ├─ produces_artifacts: ArtifactDecl[]
  ├─ guardrails: Guardrail[]
  ├─ depends_on: TaskId[]
  └─ runtime: { actual_files_modified, retry_count, agent_branch, base_conflicts }

Artifact
  ├─ id ("<task_id>.<local_name>", e.g. "T1.user_model_spec")
  ├─ type (design_doc | api_spec | schema | code_module | test_module | config | docs | report)
  ├─ producer_task_id
  ├─ summary (短文本，≤ 200 字，UI 边 tooltip 用)
  └─ file_paths: string[]        ← 物理位置，相对 worktree

Role (闭枚举 + config 覆盖)
  ├─ id, display_name, ui_color, ui_icon
  ├─ base_prompt
  ├─ default_tools: ToolName[]
  ├─ default_skills: SkillId[]
  ├─ expected_artifact_types: ArtifactType[]
  └─ default_guardrails: Guardrail[]

Skill (SKILL.md 开放标准)
  ├─ frontmatter: { name, description, tools?, compatible_roles? }
  ├─ body: string                ← 激活时注入 system prompt
  └─ resources: { scripts/, references/, assets/ }
```

### C-02: 角色 (Role) 模板

闭枚举 6 种，定义在 `<data_dir>/role_templates.json`，应用启动时加载并允许用户覆盖。

| Role | 颜色 | 主要职责 | 默认 Tools | 默认 Skills | 期望 Artifact | 默认 Guardrail |
|------|-----|---------|-----------|------------|--------------|---------------|
| **architect** | 紫 | 高层设计、模块拆分、API 契约 | read_file, list_files, search_files, write_file (限 docs/) | system_design | design_doc, api_spec | ArtifactsExist |
| **implementer** | 蓝 | 实际编码实现 | 全部 | code_implementation | code_module | ArtifactsExist + CommandPasses(build) |
| **refactorer** | 青 | 重构既有代码 | 全部 | refactoring_patterns | code_module | CommandPasses(build) + CommandPasses(test, 既有) |
| **tester** | 绿 | 编写/运行测试 | 全部 | test_authoring | test_module | CommandPasses(test, 新增) |
| **integrator** | 橙 | 接线、配置、CI/CD | 全部 | integration_glue | config, code_module | CommandPasses(build) |
| **researcher** | 灰 | 调研、原型、读外部资料 | read_file, search_files, shell_exec(只读) | research | report | ArtifactsExist |

> **设计原则**：role 是闭枚举（不可在 plan 时新增），但 `role_templates.json` 完全可覆盖（含新增 role）——开放给用户而非 LLM。

### C-03: Skill (SKILL.md 开放标准)

**完全兼容 [Cursor Skills 开放标准](https://cursor.com/docs/skills)**——用户已有的 Cursor / Claude / Codex skill 包零迁移可用。

**目录扫描顺序**（前者覆盖后者，同名 skill 高优先级生效）：

```text
1. <repo>/.miragenty/skills/    ← 项目级，Miragenty 主目录
2. <repo>/.cursor/skills/       ← 项目级，兼容 Cursor
3. <repo>/.claude/skills/       ← 项目级，兼容 Claude Code
4. <repo>/.codex/skills/        ← 项目级，兼容 Codex
5. <repo>/.agents/skills/       ← 项目级，兼容 OpenClaw
6. ~/.miragenty/skills/         ← 用户级
7. ~/.cursor/skills/            ← 用户级
8. ~/.claude/skills/            ← 用户级
9. <bundled built-in skills>    ← Miragenty 内置（应用资源目录）
```

**SKILL.md 结构**：

```text
<skill_id>/
├── SKILL.md           # 必需
├── scripts/           # 可选
├── references/        # 可选
└── assets/            # 可选
```

**SKILL.md frontmatter（YAML）**：

```yaml
---
name: rust-async-patterns           # 必需，kebab-case，与目录名一致
description: |                       # 必需，用于 Planner 自动选择
  Idiomatic async/await patterns for Rust with tokio,
  including spawn / select / timeout / cancellation.
tools: [read_file, write_file]      # 可选，激活时给 agent 的工具白名单
compatible_roles: [implementer, refactorer]  # 可选，缺省 = 全角色可用
---

# 正文（Markdown，激活时注入 system prompt）
...
```

**渐进披露**（与 Cursor / Claude 一致）：
- Planner 阶段：只读 frontmatter 的 `description`，用于决策"该任务要装哪些 skill"
- 运行时：被选中的 skill 才把 body 注入 Agent system prompt

### C-04: Artifact

**ID 命名空间**：`<task_id>.<local_name>`，如 `T1.user_model_spec`、`T3.auth_api_handler`
- `local_name` 必须 snake_case，在同一 task 内唯一
- 全局唯一性由 task_id 前缀保证
- 同类型多 producer 允许（不同 task 都产出 `code_module`），消费方按显式 ID 引用消歧

**两层存储**：

| 层 | 位置 | 内容 |
|----|------|------|
| **Metadata** | `artifacts` 表 | id, type, producer_task_id, summary, file_paths[], created_at |
| **Content** | worktree 内文件 | 由 file_paths[] 指向；merge 时随分支带入下游 |

**类型枚举（初稿）**：`design_doc / api_spec / schema / code_module / test_module / config / docs / report`

**生产方式**：Agent 调用新增的 `publish_artifact` 工具显式声明：

```json
{
  "name": "publish_artifact",
  "input": {
    "local_name": "user_model_spec",
    "type": "design_doc",
    "summary": "User model with id/email/created_at fields, Postgres backed.",
    "file_paths": ["docs/design/user-model.md"]
  }
}
```

完成时由 Engine 校验：每个 `produces_artifacts` 中声明的 artifact 必须有对应的 `publish_artifact` 调用，且 file_paths 指向的文件存在。

### C-05: Edge / Dependency

| 字段 | 类型 | 说明 |
|------|------|------|
| `task_id` | TaskId | 下游 |
| `depends_on` | TaskId | 上游 |
| `artifact_refs` | ArtifactId[] | 沿这条边流动的 artifact ID 列表 |

**渲染规则**：DAG UI 上鼠标悬停边时，tooltip 展示 `artifact_refs` 中每个 artifact 的 `summary`。

### C-06: Planner Agent Loop

**核心动机**：V2 Schema 下，单次 LLM 调用要一次性正确决策 6+ 类字段（role/skills/file_hints/artifacts/guardrails/dependencies）且彼此一致，是 anti-pattern。尤其 `file_scope_hints` 是 ground truth 问题——必须真正读过代码才能给出有意义的路径，而预先塞 `tree -L 3` + 配置摘要远远不够。

**设计原则**：把 Planner 当成一个**探索式 Coding Agent 的近亲**——先 scout 仓库，再增量 propose 任务，最后 finalize。按需 grounding，而非预先塞全量上下文。

**Planner Loop 工作流（典型）**：

```text
1. detect_tech_stack()         → "Rust + Tauri 2.0 + React 19"
2. list_directory("src", 2)    → 现有模块结构
3. search_code("auth")         → 既有相关代码
4. read_file("Cargo.toml")     → 确认依赖
5. query_skills("jwt rust", 5) → 找到 rust-jwt-patterns skill
6. propose_task({ T1, role=architect, ... })
7. propose_task({ T2, role=implementer, consumes=[T1.api_spec], ... })
8. add_dependency(T1, T2, [T1.api_spec])
9. ... 继续 propose
10. validate_plan()            → 系统返回 issue 列表
11. (如有问题) revise_task(...)
12. finalize_plan("实现 JWT 认证")
```

**完成判定**：与 Coding Agent 一致——调用 `finalize_plan` 后跑 guardrail（DAG 校验、role/skill/artifact 引用一致性、无环），失败则注入错误重试，成功则落库。

**与 Coding Agent 的差异**：

| 维度 | Coding Agent | Planner Agent |
|------|-------------|---------------|
| 工作目录 | worktree（可写） | 仓库主分支（**只读**，不允许 write_file/shell_exec） |
| 工具集 | 见 Coding 工具集 | 见 FR-05（探索 + 元数据 + 构建 + 校验 + 终止） |
| 完成 | guardrail (artifact/build/test) | guardrail (DAG 一致性) |
| 终态产物 | 文件 + artifact 入库 | tasks + task_dependencies + artifacts (decl) 入库 |
| 持久化 | agent_events | planner_steps（结构同 agent_events）|

### C-07: Pre-flight × Planner 整合

**核心设计**：Pre-flight 与 Planner 是**两个 Agent Loop**，通过 **Mission Contract** 串联。Mission 创建时即决定走哪条路径。

**两条路径**：

```text
Mission Creation 弹窗
  │
  ├─ ① title + description
  ├─ ② repo_origin: ◯ from-scratch  ◯ from-existing  (必选, FR-18)
  └─ ③ repo_path:
       ├─ from-scratch  → 显示「将自动创建 ~/miragenty-workspaces/<slug>-<id>/」
       └─ from-existing → 选择目录按钮
  │
  ▼
INSERT missions (repo_origin, repo_path, status='draft')
  │
  ▼
┌─────────────── Path A: 不走 Pre-flight ───────────────┐
│ 用户直接点 "Plan Mission"                              │
│   → plan_mission(mission_id, repo_path, contract_id=None) │
│   → Planner Agent Loop                                 │
└────────────────────────────────────────────────────────┘
                 OR
┌─────────────── Path B: 走 Pre-flight (FM-10) ─────────┐
│ 用户进入 Pre-flight 对话                                │
│   ├─ 若 repo_origin=from-existing:                     │
│   │    Pre-flight Agent 装载只读探索工具 (FR-19)       │
│   │    → 可基于实际代码提问，contract 更准确            │
│   └─ 若 repo_origin=from-scratch:                      │
│        Pre-flight 纯需求层对话                          │
│   ├─ 多轮对话 → 构建 Contract (Scope/Constraints/...)  │
│   └─ 用户点 "Sign & Plan"                              │
│       → sign_contract(mission_id) (FR-22, 重构为薄壳)  │
│         ├─ mark contract = signed                      │
│         └─ plan_mission(mission_id, repo_path,         │
│                          contract_id=Some(...))        │
│         → Planner Agent Loop（接收 Contract）          │
└────────────────────────────────────────────────────────┘
                 │
                 ▼
        Planner Agent Loop (统一入口)
        ├─ 若 contract 存在:
        │    system prompt 注入 [Contract Hard Constraints]
        │      = scope[] + exclusions[]    (FR-20, 双层注入策略 C)
        │    工具集增加 get_contract / get_contract_section
        │    Guardrail 增加 ScopeCoverage + ExclusionsUntouched (FR-21)
        │
        ├─ 探索 → propose_task → validate → finalize_plan
        └─ 持久化 DAG → mission.status = 'planned'
```

**Contract 在 Planner Loop 内的角色**：

| 字段 | 注入方式 | 理由 |
|------|---------|------|
| `scope[]` | system prompt 全文 | 硬约束，必须始终在视野 |
| `exclusions[]` | system prompt 全文 | 硬约束，必须始终在视野 |
| `constraints[]` | 工具按需 (`get_contract_section`) | 实施细节，相关 task 触发时再查 |
| `assumptions[]` | 工具按需 | 同上 |
| `budget_usd` / `quality_threshold` / `max_duration_hours` | 工具按需 | 数值字段，guardrail 阶段使用 |

**Contract Coverage Guardrail**（仅 contract-aware Planner 启用，FR-21）：

| Guardrail | 实现 | 失败动作 |
|-----------|------|---------|
| **ScopeCoverage**（强制） | LLM Judge：把全部 task description + scope items 喂模型，要求返回 `coverage: { scope_id: [task_ids] }`，缺失任一 scope 即失败 | 注入 `[Coverage Failed: scope item X not covered]`，重新探索 + 补 task |
| **ExclusionsUntouched**（强制） | LLM Judge：把全部 task description + exclusions 喂模型，触碰任一 exclusion 即失败 | 注入失败信息，让 Planner 重写相关 task 或 drop |
| **BudgetEstimate**（软警告） | 估算成本 = `Σ tasks × avg_cost_per_task`（avg 取自历史数据，无数据时跳过） | 注入提醒，不阻塞 finalize |
| **DurationEstimate**（软警告） | 同上 | 同上 |
| **QualityThreshold**（软警告） | 检查 Tester role 的 task 数 ≥ 阈值要求的最小值（>=8 要求至少 1 个 Tester；>=9 至少 2 个；满分要求覆盖每个 implementer/refactorer） | 同上 |

---

## SR — Software Requirements

### 功能需求

#### FR-01: Role Template 系统

- **FR-01.1**: 后端新增 `agent/roles.rs`，定义 `Role` 枚举（6 种）和 `RoleTemplate` 结构体
- **FR-01.2**: 应用启动时从 `<data_dir>/role_templates.json` 加载用户覆盖；文件不存在则用内置默认值
- **FR-01.3**: 提供 `get_role_templates` Tauri command，返回所有 role 的元数据（含 ui_color/ui_icon）供前端渲染
- **FR-01.4**: `Role` 枚举允许通过 `role_templates.json` 增加新 role，但 Planner prompt 中始终只列举当前已加载的 role
- **FR-01.5**: 修改 `role_templates.json` 后需要重启应用生效（v1 不做热加载）

#### FR-02: Skill Registry

- **FR-02.1**: 后端新增 `skills/registry.rs`，启动时按 C-03 定义的顺序扫描 9 个目录
- **FR-02.2**: 解析 SKILL.md 的 YAML frontmatter，校验必需字段 `name` / `description`；非法 skill 跳过并 warn
- **FR-02.3**: 同名 skill 按目录优先级覆盖（项目级 > 用户级 > 内置）
- **FR-02.4**: 提供 `list_skills` Tauri command，返回 `SkillMeta[]`（仅 frontmatter，不含 body）供 UI 列表展示
- **FR-02.5**: 提供 `get_skill_body(skill_id)` 内部 API（不暴露给前端），运行时 Engine 在装载 skill 时调用
- **FR-02.6**: Skill body 在 Agent system prompt 中以 `[Skill: <name>]\n<body>\n[/Skill]` 块注入
- **FR-02.7**: 内置 skill 集（Phase 1 提供）：
  - `system-design`：架构设计模式与决策记录
  - `code-implementation`：通用编码规范（命名、错误处理、日志）
  - `refactoring-patterns`：重构 catalog
  - `test-authoring`：测试设计原则与覆盖率指引
  - `integration-glue`：CI / 配置 / 部署衔接
  - `research`：调研报告写法

#### FR-03: Artifact 数据模型与生命周期

- **FR-03.1**: 新增 `artifacts` 表（见数据需求）
- **FR-03.2**: 新增 `publish_artifact` 工具（`tools/definitions.rs`），由 Agent 在产出文件后调用
- **FR-03.3**: `tools/executor.rs` 处理该工具时：
  - 校验 `file_paths` 全部存在于 worktree
  - 校验 `local_name` 与 task 的 `produces_artifacts` 声明匹配
  - 写入 `artifacts` 表
  - 触发 `artifact-published` 事件
- **FR-03.4**: `task_dependencies` 表新增 `artifact_refs TEXT NOT NULL DEFAULT '[]'` 列（JSON 数组）
- **FR-03.5**: `consumes_artifacts` 在 Agent 启动时由 Engine 解析为"上游 artifact 的 summary 注入"
- **FR-03.6**: 任意一个声明的 artifact 未在 task 完成时实际 publish，guardrail `ArtifactsExist` 失败

#### FR-04: PlannerTask Schema 扩展

`PlannerTask` 结构体扩展为：

```rust
pub struct PlannerTask {
    pub id: String,
    pub title: String,
    pub description: String,
    pub complexity: String,           // 保留兼容
    pub expected_output: String,      // 新增：验收契约
    pub role: String,                 // 新增：role id
    pub additional_skills: Vec<String>, // 新增：skill id 列表
    pub file_scope_hints: FileScopeHints, // 新增
    pub produces_artifacts: Vec<ArtifactDecl>, // 新增
    pub consumes_artifacts: Vec<String>,       // 新增：artifact id 列表
    pub guardrails: Vec<GuardrailDecl>,        // 新增
    pub depends_on: Vec<String>,
}
```

- **FR-04.1**: 校验规则（在 `propose_task` 与 `finalize_plan` 两处执行）：
  - `role` 必须存在于已加载的 RoleTemplate 中
  - `additional_skills` 中的每个 id 必须在 SkillRegistry 中存在；且 skill 的 `compatible_roles`（若声明）必须包含当前 role
  - `produces_artifacts[].local_name` snake_case 校验，task 内唯一
  - `consumes_artifacts` 中的每个 ID 必须已被上游 task 通过 `propose_task` 声明产出
  - 至少一个上游 task 包含被消费的 artifact（用于推导 edge）
- **FR-04.2**: `task_dependencies` 写入时，根据 `task.consumes_artifacts` 推导每条 edge 的 `artifact_refs`
- **FR-04.3**: 保留 `complexity` 字段用于排序与展示，不再驱动调度决策

#### FR-05: Planner Agent Loop 工具集

> **范式变更**：v2.0 的"预先塞全量上下文"被 v2.1 的"按需调用工具"取代。Planner 是一个 read-only 的探索式 Agent，工作目录为仓库主分支，禁止 write_file / shell_exec / git 写操作。

- **FR-05.1**: `plan_mission` 命令必需 `repo_path` 参数；启动 Planner Agent Loop（不再调用 `call_planner` 单次接口）
- **FR-05.2**: Planner 工具集定义于 `agent/planner_tools.rs`，分 5 类：

  **A. 探索类（Read-only filesystem）**
  ```rust
  list_directory(path: String, max_depth: u32 ≤ 3) -> DirTree
  read_file(path: String, line_range: Option<(u32, u32)>) -> String  // 文件 ≤ 200KB
  search_code(query: String, file_pattern: Option<String>) -> Vec<Match>  // ripgrep
  detect_tech_stack() -> TechStackReport  // 解析 package.json/Cargo.toml/pyproject.toml/go.mod/pom.xml
  ```

  **B. 元数据类（按需查询，避免预先全塞）**
  ```rust
  list_roles() -> Vec<RoleMeta>  // 全部 role + ui_color/icon/expected_artifact_types
  query_skills(intent: String, top_k: u32 ≤ 10) -> Vec<SkillMeta>  // 关键词匹配（Q-P1 决策 A）
  get_skill_detail(skill_id: String) -> SkillMeta  // frontmatter 全部字段

  // 仅 contract-aware Planner（FR-20）启用：
  get_contract() -> ContractData  // 全部字段
  get_contract_section(section: String) -> Vec<String>  // scope|constraints|exclusions|assumptions
  ```

  **C. 构建类（增量构造 DAG，每次调用立即校验）**
  ```rust
  propose_task(task: PlannerTask) -> TaskId  // 失败抛出明确错误供 LLM 修正
  add_dependency(from: TaskId, to: TaskId, artifact_refs: Vec<ArtifactId>) -> ()
  revise_task(task_id: TaskId, changes: PartialPlannerTask) -> ()
  drop_task(task_id: TaskId) -> ()
  ```

  **D. 校验类（模型主动调用）**
  ```rust
  validate_plan() -> Vec<ValidationIssue>
  // 检查项：环、悬空 artifact 引用、孤儿 task、role/skill 兼容性、artifact ID 命名
  ```

  **E. 终止类**
  ```rust
  finalize_plan(mission_title: String) -> ()
  // 触发 guardrail（FR-09）：DAG 一致性校验全部 pass → 持久化 → 完成
  ```

- **FR-05.3**: `read_file` 与 `list_directory` 必须遵守 `.gitignore` + 内置黑名单（`node_modules / target / .git / dist / build / .worktrees`）；越权访问返回错误
- **FR-05.4**: `search_code` 内部调用 `rg`，超时 5s，结果上限 100 行，超出截断
- **FR-05.5**: `query_skills` 实施（Q-P1 决策 A）：
  - 在所有已注册 skill 的 `description + name` 上做不区分大小写的关键词匹配
  - 多关键词时取交集；返回结果按"在 description 中出现的关键词数量"降序
  - 若全无匹配 → 返回 description 长度排序的前 K 个 skill 作为 fallback
- **FR-05.6**: `fetch_url` 工具（Q-P2 决策 B + 白名单收紧）

  **工具签名**：
  ```rust
  fetch_url(url: String) -> String  // 返回 markdown 化内容，上限 50KB
  ```

  **安全策略**：
  1. **强制黑名单（无视白名单）**：`localhost`、`127.0.0.0/8`、`10.0.0.0/8`、`172.16.0.0/12`、`192.168.0.0/16`、`169.254.0.0/16`、`::1`、`fc00::/7`，以及 `file://` / `ftp://` / `gopher://` 等非 HTTPS scheme（仅允许 `https://`）
  2. **白名单**：`config.json.planner_fetch_allowlist: string[]`（默认 `[]`），存储顶级域名（如 `github.com`，子域名自动匹配）
  3. **首次域名确认流程**：
     - Planner 调用 `fetch_url("https://example.com/spec.md")`
     - 域名 `example.com` 不在白名单 → 工具暂停执行 → 触发 `planner-fetch-confirmation` 事件，payload `{ planner_session_id, url, domain }`
     - 前端弹出确认框：「Planner 想访问 `example.com`」+ 三选一按钮：「允许此次」/「永久允许（加入白名单）」/「拒绝」
     - 通过新 command `confirm_planner_fetch(planner_session_id, decision)` 回传决策
     - 「拒绝」→ 工具返回错误 "user denied access to domain"
     - 「允许此次」→ 本次 plan 会话内该域名免再确认；plan 结束后清除
     - 「永久允许」→ 写入 `planner_fetch_allowlist` 配置
  4. **理由**：plan 阶段用户在屏幕前等待，可接受单次确认；execute 阶段 coding agent **不分配** `fetch_url` 工具，保持完全无人值守（与 US-10 一致）
  5. **rate limit**：单次 plan 最多 10 次 `fetch_url` 调用，超出工具返回错误
  6. **超时**：单次请求 10s
- **FR-05.7**: 工具调用全过程持久化到 `planner_steps` 表（见数据需求），并通过 `planner-step` 事件流式推送前端

#### FR-06: Planner System Prompt 重写（Agent Loop 版）

- **FR-06.1**: 完全重写 `PLANNER_SYSTEM_PROMPT`，结构如下：

  **§ 角色与目标**
  > 你是 Miragenty 的 Mission Planner Agent。你的目标是把用户的需求拆解成一个可执行的 Task DAG。

  **§ 工作流程**
  1. **Scout（探索）**：用 `detect_tech_stack` / `list_directory` / `read_file` / `search_code` 充分了解目标仓库
  2. **Skill 检索**：根据需求关键词调用 `query_skills`，必要时 `get_skill_detail`
  3. **Propose（增量构造）**：用 `propose_task` 一次提交一个 task，立即处理校验错误
  4. **Wire（连接依赖）**：用 `add_dependency`，依据 `consumes_artifacts` 关系
  5. **Validate（自检）**：调用 `validate_plan`，根据返回的 issue 修正
  6. **Finalize**：所有 issue 解决后调用 `finalize_plan`

  **§ 决策原则**
  - **任务自包含性**：每个 task 的 `description` 必须自包含；`expected_output` 必须可验证
  - **Role 选择**：根据 task 性质从 `list_roles` 返回的角色中选一个，参考各 role 的 `expected_artifact_types`
  - **Skill 注入**：role 默认 skill 不够时才用 `additional_skills` 加；优先调用 `query_skills` 而非凭空猜
  - **Artifact 设计**：`local_name` 用 snake_case 描述产物语义（不要用 `output` / `result` 这类无意义命名）
  - **依赖语义**：edge = artifact 流；不消费上游产物就不建依赖
  - **File Scope Hints**：基于实际探索结果给 `definite`，不确定放 `possible`；不要凭直觉硬猜
  - **粒度**：每 task 对应 20-50 步 Coding Agent 工作量

  **§ 工具调用纪律**
  - 一次只做一件事，不要并行调用多个修改类工具
  - 校验失败 → 修正后重试，不要继续推进
  - 探索类工具调用次数无上限，但避免重复 read 同一文件
  - `fetch_url` 谨慎使用，仅在用户描述里明确给出 URL 或文档链接时调用

  **§ 终止条件**
  - 调用 `finalize_plan(mission_title)` 仅当 `validate_plan` 返回空 issue 列表
  - Planner 自身有 step 与时间限制（FR-11）

- **FR-06.2**: Planner 与 Coding Agent **共享 `AgentEngine` 主循环**（FR-09 的 guardrail 模式复用），仅工具集和 system prompt 不同
- **FR-06.3**: Planner 完成 guardrail 即"调用 `finalize_plan` + DAG 校验全 pass"，失败注入错误重试（重试预算同 FR-09.5，默认 3 次）

#### FR-07: 增量 Worktree（重大架构变更）

- **FR-07.1**: 新增 git 操作 `prepare_task_base(task_id) -> branch_name`：
  1. 查询 task 的所有**直接父** task（已 completed）
  2. 创建新分支 `task-base/<task_id>` 自当前 main HEAD（每次新建，避免历史污染）
  3. 按拓扑后序合并所有直接父的 `agent/<parent_id>` 分支：`git merge agent/<parent_id> --no-ff -X theirs`
  4. 记录每次合并是否产生冲突，写入 `task_base_conflicts` 表
- **FR-07.2**: 调用 LLM 解冲突时（FR-08）若结果不可用，最终落地 `-X theirs` 并将冲突清单作为 task 启动时的额外上下文提示注入 Agent
- **FR-07.3**: `dispatch_task` 改为先调用 `prepare_task_base`，再 `git worktree add <path> <task-base/<task_id>>`，最后 `git checkout -b agent/<task_id>`
- **FR-07.4**: 上游 task 失败 → 该分支跳过（不合入），但仍允许该 task 的 base 构建成功（其它已完成父合入）；如果**所有直接父**都失败 → 当前 task 不调度，标记 blocked 直至 mission 终态判定为 failed
- **FR-07.5**: `merge_completed_mission` 简化为"合并所有 frontier task"（无后继且 completed）：
  - 拓扑后序排序 frontier
  - 依次 `git merge agent/<task_id>` 到 main，冲突走 FR-08 三层策略
  - 现有"按全 task 拓扑序合并"逻辑废弃
- **FR-07.6**: Phase 2 提供向后兼容开关 `mission.use_incremental_worktree`（默认 true，可在 mission 创建时关闭走旧逻辑）

#### FR-08: 三层冲突合并

- **FR-08.1**: 引入 `MergeStrategy` 配置：
  ```rust
  enum MergeStrategy { LlmResolve, Theirs, Ours }
  ```
  - 默认 `LlmResolve`
  - Mission 级配置：`missions.merge_strategy`
  - Task 级覆盖（hint）：`tasks.merge_strategy_hint`
- **FR-08.2**: 三层降级流程（实施于 `merge_with_strategy(branch, strategy)`）：
  1. **L1**：`git merge <branch>` 默认行为，无冲突 → 完成
  2. **L2 启发式**：检测冲突类型；只是 import 顺序 / 空行 / 空格差异 → `git checkout --theirs <file>` + 自动 git add
  3. **L3 LLM 解冲突**（仅当 strategy = LlmResolve）：
     - 收集每个冲突文件的 `<<<<<<< | ======= | >>>>>>>` 标记块 + 文件全文 + 双方 task 的 description / expected_output
     - 调用 LLM 产出合并版本
     - 写回文件，自动 git add
     - 跑 build guardrail（若 mission 配置了 build 命令）
     - 失败 → 回滚到 L2（`-X theirs`）+ 写入 `merge_records.fallback_reason`
- **FR-08.3**: 所有合并结果记录到新表 `merge_records`：
  ```text
  id, mission_id, source_branch, target_branch,
  strategy_attempted, final_strategy, conflicted_files,
  llm_resolution_succeeded, build_passed_after_llm,
  ts
  ```
- **FR-08.4**: 交付面板显示 `merge_records` 中 LLM 解冲突的文件列表，标记"⚠ AI 解决，建议复核"
- **FR-08.5**: LLM 解冲突的 prompt 与模型配置独立，可在 `config.json.merge_resolver` 中指定（默认复用 `default_model`，但允许覆盖为更强模型）

#### FR-09: Guardrail 完成检测（替代无 tool_use 判定）

- **FR-09.1**: 引入 `Guardrail` 枚举：
  ```rust
  enum Guardrail {
      ArtifactsExist,
      CommandPasses { cmd: String, timeout_sec: u32, working_dir: Option<String> },
      FilesNonEmpty { globs: Vec<String> },
      LlmJudge { criteria: String, model: Option<String> },
      // Planner 专属（FR-21）
      ScopeCoverage,           // 强制：每个 scope item 必须被覆盖
      ExclusionsUntouched,     // 强制：没有 task 触碰 exclusions
      BudgetEstimate { hard: bool },     // hard=false 仅警告
      DurationEstimate { hard: bool },
      QualityThreshold { hard: bool },
  }
  ```
- **FR-09.2**: Guardrail 来源三层（合并去重）：
  1. Role template 默认 guardrails
  2. Skill frontmatter 可声明 `guardrails: [...]`
  3. Task 显式声明 `guardrails: [...]`（Planner 输出）
- **FR-09.3**: 完成触发：Agent 调用新增的 `task_complete` 工具（参数：`summary: string`）
  - 废弃"无 tool_use 即完成"
  - 若 `max_consecutive_no_tool` ≥ 3 仍未调用 `task_complete` → 注入提示："请使用工具或调用 task_complete 结束"
- **FR-09.4**: 调用 `task_complete` 后顺序执行所有 guardrail：
  - 全部通过 → task `completed`，`completion_summary` 入库
  - 任一失败 → 把失败信息（guardrail 名 + 错误详情）追加为新 user message，重新进入 LLM 循环
- **FR-09.5**: 重试预算：`task.guardrail_retry_budget`（默认 3，role template 可覆盖；Tester role 默认 5）
  - 重试次数耗尽 → task `failed`，已修改文件仍 commit
- **FR-09.6**: Guardrail 执行细节：
  - `ArtifactsExist`：校验 `task.produces_artifacts` 中每项都已通过 `publish_artifact` 写入且 file_paths 存在
  - `CommandPasses`：在 worktree 内 `tokio::process::Command` 执行；非 0 退出码 = 失败；输出捕获 stdout+stderr 各前 2000 字符作为错误信息
  - `FilesNonEmpty`：glob 匹配后检查文件大小 > 0
  - `LlmJudge`：把 `expected_output` + diff（`git diff main..agent/<task_id>` 截断到 8000 字符）+ criteria 喂 LLM，要求返回 `{ pass: bool, reason: string }`

#### FR-10: Codebase Intelligence 注入

- **FR-10.1**: Agent system prompt 启动时自动注入两个上下文块（必注入，L1）：
  - `[Project Structure]`：`tree -L 3` 输出（同 FR-05.2 忽略规则）
  - `[Tech Stack]`：自动检测的语言 + 框架摘要（如 "Rust + Tauri 2.0 + React 19 + tokio + git2"）
- **FR-10.2**: 条件注入（L2）：
  - 当 task 有上游：注入 `[Upstream Context]` 块，包含每个上游 task 的 `completion_summary` + 该 task 产出的 artifact 列表（`<id>: <type> @ <file_paths>`）
  - 当 task base 构建有冲突：注入 `[Base Conflicts]` 块，列出冲突文件 + 简要说明 "上游 X 与 Y 在这些文件上有冲突，已用启发式 / LLM 解决，请注意"
- **FR-10.3**: Skill body 注入（FR-02.6）作为独立块 `[Skills]`
- **FR-10.4**: 上下文块的总注入量上限 12000 tokens，超出按 `[Skills] > [Upstream Context] > [Project Structure] > [Tech Stack] > [Base Conflicts]` 优先级保留并截断其它

#### FR-11: Agent 超时与步数限制

- **FR-11.1**: 配置项 `max_agent_steps`（默认 50）、`agent_timeout_seconds`（默认 600）
- **FR-11.2**: `AgentEngine::run` 用 `tokio::time::timeout` 包裹整个执行循环
- **FR-11.3**: 步数接近上限（剩 5 步）→ 注入提示："剩余 N 步，请尽快收尾并调用 task_complete"
- **FR-11.4**: 超时 / 超步触发：
  - status = failed，原因 `timeout` / `max_steps_exceeded`
  - 已修改文件仍 commit（保留产出）
  - 触发 `task-status-changed` 事件

#### FR-12: 主分支自动检测

- **FR-12.1**: `WorktreeManager::detect_main_branch()` 按优先级：
  1. `refs/remotes/origin/HEAD` 指向的分支名
  2. 依次检测 `main`、`master`、`develop` 是否存在
  3. fallback 为当前 HEAD 所在分支
- **FR-12.2**: 检测结果缓存在 `Mission` 内，避免每次 merge 重新检测
- **FR-12.3**: 启动 mission 时日志记录使用的主分支名

#### FR-13: Mission 级并发隔离

- **FR-13.1**: `count_running_agents` 增加 `mission_id` 参数版本，仅统计该 mission 下的 running agent 数
- **FR-13.2**: 每个 mission 独立使用 `max_concurrent_agents` 配额
- **FR-13.3**: 全局总数仍可通过 `Scheduler::active_count()` 监控（不作限制）

#### FR-14: 终态交付面板

- **FR-14.1**: Mission 状态变为 `completed` 时触发 `mission-delivered` 事件，payload 包含：
  ```typescript
  {
    mission_id: string;
    workspace_path: string;
    artifacts: Array<{ id, type, summary, file_paths }>;
    diff_stats: { files_changed, additions, deletions };
    llm_resolved_files: string[];  // 从 merge_records 聚合
  }
  ```
- **FR-14.2**: 前端 `MissionsView` 监听该事件，弹出 `MissionDeliveryPanel` 组件，包含：
  - 工作区路径 + `Open in Editor` / `Open in Terminal` / `View Diff` 按钮
  - Artifacts 按 type 分组展示
  - LLM 解冲突文件标红，提示复核
  - Chat 输入框（→ FR-15）
- **FR-14.3**: `Open in Editor` 通过 Tauri 的 `shell.open` API 调用系统默认编辑器（用户可在 config 中指定，默认 `code`）

#### FR-15: Follow-up Chat（B 方案）

- **FR-15.1**: 新增 `mission_chats` 表（见数据需求），每 mission 一个独立会话
- **FR-15.2**: 新增 Chat Agent，使用与 Coding Agent 相同的工具集，但工作目录直接是 main 分支（不开 worktree）
- **FR-15.3**: Chat Agent system prompt 包含：
  - Mission summary + 全部 task 的 completion_summary + artifact 列表
  - 当前 main HEAD 的项目结构
  - **关键指令**：「评估用户请求规模。若 ≤ 3 文件 / ≤ 30 行改动 / 无新建模块 → 直接执行；否则调用 `propose_followup_mission` 工具提议升级」
- **FR-15.4**: 新增 `propose_followup_mission` 工具，参数：
  ```typescript
  {
    title: string;        // 子 mission 标题
    rationale: string;    // 为何需要升级（向用户说明）
    estimated_tasks: number;  // 预估 task 数
  }
  ```
  调用后：
  - 触发 `followup-proposed` 事件，前端弹窗显示 rationale + estimated_tasks + 「确认走 plan 流程」/「取消，让 Chat 直接做」两个按钮
  - 用户确认 → 自动调用 `plan_mission`，将原 mission 的所有 artifact summary + 用户请求作为 description 传入；新 mission 的 `parent_mission_id` 关联当前 mission
  - 用户拒绝 → Chat Agent 收到通知，强制直接执行
- **FR-15.5**: Chat 操作（含 chat agent 直接执行）的修改直接 commit 到 main（不开 worktree），但每次 commit 前自动 `git diff` 校验，超过 30 行变更时强制走 propose 流程
- **FR-15.6**: Chat 会话与 follow-up missions 持久化；下次打开 mission 仍可见

#### FR-16: 前端 DAG UI 增强

- **FR-16.1**: `TaskDAG.tsx` 节点渲染：
  - 节点边框颜色 = role 的 `ui_color`
  - 节点左上角显示 role 的 `ui_icon`（emoji 或 SVG）
  - 节点底部显示 `produces_artifacts` 数量徽标
- **FR-16.2**: 边渲染：
  - 边上中点显示 artifact 数量小标签（如 `2 artifacts`）
  - 鼠标悬停 → tooltip 列出每个 artifact 的 type + summary
- **FR-16.3**: 节点 tooltip 增加：role + skills + file_scope_hints + expected_output
- **FR-16.4**: `TaskEditDialog.tsx` 支持编辑新增的所有字段（role 下拉、skills 多选、artifacts 表单）

#### FR-17: 前端事件封装补全

- **FR-17.1**: `events.ts` 新增封装：
  - `onMissionMergeProgress`
  - `onMissionMergeCompleted`
  - `onArtifactPublished`
  - `onMissionDelivered`
  - `onFollowupProposed`
  - `onChatStreamChunk`
  - `onPlannerStep`
  - `onPlannerFetchConfirmation`
- **FR-17.2**: 对应的 store action 在 `agent-store.ts` 中实现

#### FR-18: Mission 创建强制采集 repo_origin + repo_path

- **FR-18.1**: `missions` 表新增 `repo_origin TEXT NOT NULL DEFAULT 'from_existing' CHECK (repo_origin IN ('from_scratch','from_existing'))`
- **FR-18.2**: `missions` 表新增 `repo_path TEXT`（已在 v2.0 数据需求中加入；v2.2 改为创建 mission 时必填）
- **FR-18.3**: 新建 mission 弹窗增加：
  - **Origin 单选**：「从已有项目开始」 / 「从零开始新项目」
  - **Path 区域**（根据 origin 显示）：
    - `from_existing` → 「选择目录」按钮（Tauri `dialog.open(directory)`）+ 路径展示 + 校验：必须存在、必须是 git repo（缺则提示「初始化 git？」）
    - `from_scratch` → 自动生成预览 `~/miragenty-workspaces/<title-slug>-<short-id>/`（只读展示，复用 `get_default_workspace_path` 命令）
- **FR-18.4**: `create_mission` command 接受 `{ title, description, repo_origin, repo_path }`：
  - `from_existing` 路径不存在或非 git repo → 拒绝创建
  - `from_scratch` → 自动 `mkdir -p` + `git init` + 创建初始空提交（避免后续 worktree 操作失败）
- **FR-18.5**: 已有 mission（v2.2 之前创建的）`repo_origin` 默认 `from_existing`，`repo_path` 若缺失则禁止启动 plan / execute，UI 弹窗补选

#### FR-19: Pre-flight 只读探索能力（仅 from_existing）

- **FR-19.1**: Pre-flight Agent（FM-10）增加只读探索工具，**仅当 `mission.repo_origin = 'from_existing'`** 时启用：
  - `list_directory(path, max_depth ≤ 3)`
  - `read_file(path, line_range?)`（文件 ≤ 200KB）
  - `search_code(query, file_pattern?)`（ripgrep，超时 5s）
  - `detect_tech_stack()` 
- **FR-19.2**: 工具实施**复用** `agent/planner_tools.rs` 中的 A 类工具（不重复实现）
- **FR-19.3**: Pre-flight 不允许 `propose_task` / `add_dependency` / `validate_plan` / `finalize_plan` / `fetch_url` 等任何写类工具
- **FR-19.4**: Pre-flight system prompt 在 `from_existing` 模式追加：
  > 你可以使用 `list_directory` / `read_file` / `search_code` / `detect_tech_stack` 探索目标仓库，基于实际代码提出更精准的 contract 问题。例如：发现项目用 sqlx → 提问"是否沿用 sqlx 还是切换 ORM"；发现已有 `src/auth/` → 提问"是新增 auth 模块还是扩展既有"
- **FR-19.5**: Pre-flight 探索步骤同样写入 `planner_steps`（共用表，通过 `session_kind = 'preflight' | 'planner'` 区分）；`planner_sessions` 同步增加 `kind` 字段

#### FR-20: Contract 在 Planner Loop 内的双层注入

- **FR-20.1**: `plan_mission` 接受可选 `contract_id`；存在时启用 contract-aware 模式
- **FR-20.2**: contract-aware 模式下，Planner system prompt 末尾追加 `[Contract Hard Constraints]` 块：
  ```text
  [Contract Hard Constraints — must be respected]

  ## Scope (MUST cover ALL of these):
  - <scope item 1>
  - <scope item 2>
  ...

  ## Exclusions (MUST NOT touch ANY of these):
  - <exclusion item 1>
  ...

  使用 get_contract_section 工具查询 constraints / assumptions / 数值约束的完整内容。
  [End Contract]
  ```
- **FR-20.3**: 工具集启用 `get_contract` / `get_contract_section`（FR-05.2 B 类已声明）
- **FR-20.4**: Constraints / Assumptions 不进 system prompt，避免 token 浪费；模型按需查
- **FR-20.5**: 数值字段（budget / quality / duration）在 guardrail 阶段消费（FR-21）；Planner 推理过程中如需引用，通过 `get_contract` 查

#### FR-21: Coverage / Exclusion Guardrails

- **FR-21.1**: 引入 5 类 Planner 专属 guardrail（已在 FR-09.1 声明）
- **FR-21.2**: 仅当 `plan_mission(contract_id=Some(...))` 时启用；无 contract 的 plan 跳过
- **FR-21.3**: 强制 guardrail（`hard=true`，等价于必通过）：
  - **ScopeCoverage**：调用 LLM Judge：
    ```text
    SCOPE ITEMS:
    1. <scope_1>
    2. <scope_2>
    ...

    PROPOSED TASKS:
    T1: <title> — <description first 200 chars>
    T2: ...

    Return JSON: { "uncovered_scope_ids": [int], "rationale": string }
    ```
    `uncovered_scope_ids` 非空 → 失败
  - **ExclusionsUntouched**：类似结构，返回 `{ violating_task_ids: [string], details: string }`，非空 → 失败
- **FR-21.4**: 软警告 guardrail（`hard=false`，失败仅注入提示，不阻塞 finalize）：
  - **BudgetEstimate**：估算 = `Σ tasks × avg_cost_per_task`（avg 由历史数据推导，无数据则取默认 $0.50）；超 budget × 1.2 → 警告
  - **DurationEstimate**：估算 = `Σ tasks × avg_duration_seconds`（同上，默认 600s）；超 max_duration × 1.2 → 警告
  - **QualityThreshold**：根据阈值映射要求的 Tester role 数：
    - `>= 8.0`：至少 1 个 tester role 的 task
    - `>= 9.0`：至少 2 个 tester role 的 task
    - `= 10.0`：每个 implementer / refactorer 至少有一个对应的 tester task（依赖关系上）
    - 不满足 → 警告
- **FR-21.5**: Guardrail 失败信息以 `[Coverage Failed: scope item "X" not covered by any task]` 形式注入下一轮 user message
- **FR-21.6**: 强制 guardrail 失败计入 retry_count；软警告不计入

#### FR-22: sign_contract 重构

- **FR-22.1**: `sign_contract` command 重构为薄壳，去除内部 LLM 调用：
  ```rust
  async fn sign_contract(mission_id) -> PlanMissionResponse {
      // 1. 校验 contract（≥ 1 scope item）
      // 2. 标记 contract.status = 'signed'
      // 3. 委托给 plan_mission：
      plan_mission(mission_id, repo_path, contract_id = Some(contract_id))
  }
  ```
- **FR-22.2**: 删除 `commands/preflight.rs::stream_planner_call_for_contract` 与相关代码
- **FR-22.3**: 删除 `agent/planner.rs::build_contract_aware_planner_prompt`（功能由 FR-20.2 在 Planner Loop 内动态拼装替代）
- **FR-22.4**: `sign_contract` 与 `plan_mission` 走同一份 Planner Agent Loop，确保所有未来 Planner 改进自动覆盖两条入口
- **FR-22.5**: 前端 `signContract` 调用契约不变（仍返回 `PlanMissionResponse`），向后兼容

### 非功能需求

| ID | 项 | 指标 |
|----|----|------|
| NFR-01 | Skill 注册扫描时间 | ≤ 500ms（≤ 100 个 skill） |
| NFR-02 | Planner Agent Loop 单次 plan 总耗时 | ≤ 5min（含全部工具调用与 LLM 调用） |
| NFR-02b | Planner 单次工具调用（除 LLM 类）耗时 | ≤ 5s |
| NFR-03 | `prepare_task_base` 单 task | ≤ 5s（≤ 5 个直接父） |
| NFR-04 | LLM 解冲突单文件 | ≤ 30s（含 build 校验） |
| NFR-05 | Guardrail 总执行时间 | ≤ guardrail 最长项 + 5s 调度开销 |
| NFR-06 | Agent 完成判定准确率 | ≥ 95%（误判率 ≤ 5%） |
| NFR-07 | Codebase intelligence 上下文 | 总 token ≤ 12000 |
| NFR-08 | 全部改动 | 通过 `cargo test` + `pnpm test` + `pnpm build` |

### 接口需求

**变更 / 新增的 Tauri Commands**：

| Command | 状态 | 说明 |
|---------|------|------|
| `create_mission` | 变更 | 新增 `repo_origin` (必需) + `repo_path` (必需)；`from_scratch` 自动创建目录 + git init (FR-18) |
| `plan_mission` | 变更 | `repo_path` 改为必需，新增可选 `contract_id`；启动 Planner Agent Loop |
| `sign_contract` | 变更 | 重构为薄壳，内部委托 `plan_mission(contract_id=Some(...))` (FR-22) |
| `confirm_mission` | 不变 | — |
| `start_mission_execution` | 变更 | 内部启用增量 worktree 流程 |
| `get_mission_detail` | 变更 | TaskInfo 含全部新字段 |
| `add_task` / `update_task` | 变更 | 支持新字段 |
| `get_role_templates` | 新增 | 返回所有 role 元数据 |
| `list_skills` | 新增 | 返回 SkillMeta[]（不含 body） |
| `get_merge_records` | 新增 | 查询某 mission 的 merge 记录 |
| `get_artifacts` | 新增 | 查询某 mission 的所有 artifact |
| `get_planner_steps` | 新增 | 查询某次 plan session 的探索步骤（用于回放 / 调试） |
| `confirm_planner_fetch` | 新增 | Planner `fetch_url` 域名确认决策回传 (FR-05.6) |
| `send_chat_message` | 新增 | 用户向 follow-up chat 发消息 |
| `confirm_followup_mission` | 新增 | 用户确认 / 拒绝 propose 的 mini-mission |
| `get_chat_history` | 新增 | 加载 mission 的 chat 历史 |

**新增事件**：

| Event | Payload | 说明 |
|-------|---------|------|
| `planner-step` | `{ session_id, step_no, kind, tool_name?, tool_args?, tool_result?, text? }` | Planner Agent Loop 单步事件（流式） |
| `planner-fetch-confirmation` | `{ session_id, url, domain }` | Planner 请求确认非白名单域名访问 (FR-05.6) |
| `artifact-published` | `{ mission_id, task_id, artifact_id, type, summary }` | Agent 发布 artifact |
| `task-base-prepared` | `{ task_id, base_branch, conflicts: string[] }` | task 增量 base 构建完成 |
| `merge-decision` | `{ mission_id, file, layer: "L1"\|"L2"\|"L3", succeeded }` | 单文件合并决策 |
| `mission-delivered` | 见 FR-14.1 | mission 完成交付 |
| `followup-proposed` | `{ mission_id, title, rationale, estimated_tasks }` | Chat agent 提议升级 |
| `chat-stream-chunk` | `{ mission_id, chunk, kind }` | Chat 流式输出 |
| `guardrail-result` | `{ task_id, guardrail, passed, reason }` | Guardrail 执行结果 |

**新增配置项**（`config.json`）：

| 配置项 | 类型 | 默认值 | 说明 |
|--------|------|--------|------|
| `max_agent_steps` | u32 | 50 | 单 Agent 最大执行步数 |
| `agent_timeout_seconds` | u64 | 600 | 单 Agent 执行超时 |
| `default_merge_strategy` | string | "llm_resolve" | "llm_resolve" \| "theirs" \| "ours" |
| `merge_resolver_model` | string? | null | LLM 解冲突专用模型，缺省复用 default_model |
| `chat_safety_diff_threshold_lines` | u32 | 30 | Chat agent 直接 commit 的行数上限 |
| `default_editor_command` | string | "code" | 终态面板"打开编辑器"使用的命令 |
| `planner_max_steps` | u32 | 80 | Planner Agent Loop 最大步数 |
| `planner_timeout_seconds` | u64 | 600 | Planner Agent Loop 总超时 |
| `planner_fetch_allowlist` | string[] | `[]` | Planner `fetch_url` 永久白名单（顶级域名） |
| `planner_max_fetches_per_session` | u32 | 10 | 单次 plan session 内 `fetch_url` 上限 |

### 数据需求

**Migration 014: orchestration enhancement schema**

```sql
-- Tasks 扩展
ALTER TABLE tasks ADD COLUMN role TEXT NOT NULL DEFAULT 'implementer';
ALTER TABLE tasks ADD COLUMN expected_output TEXT NOT NULL DEFAULT '';
ALTER TABLE tasks ADD COLUMN additional_skills TEXT NOT NULL DEFAULT '[]';
ALTER TABLE tasks ADD COLUMN file_scope_hints TEXT NOT NULL DEFAULT '{"definite":[],"possible":[]}';
ALTER TABLE tasks ADD COLUMN guardrails TEXT NOT NULL DEFAULT '[]';
ALTER TABLE tasks ADD COLUMN guardrail_retry_budget INTEGER NOT NULL DEFAULT 3;
ALTER TABLE tasks ADD COLUMN guardrail_retry_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE tasks ADD COLUMN actual_files_modified TEXT NOT NULL DEFAULT '[]';
ALTER TABLE tasks ADD COLUMN completion_summary TEXT;
ALTER TABLE tasks ADD COLUMN merge_strategy_hint TEXT;
ALTER TABLE tasks ADD COLUMN agent_branch TEXT;

-- Edges 扩展
ALTER TABLE task_dependencies ADD COLUMN artifact_refs TEXT NOT NULL DEFAULT '[]';

-- Missions 扩展
ALTER TABLE missions ADD COLUMN repo_origin TEXT NOT NULL DEFAULT 'from_existing'
    CHECK (repo_origin IN ('from_scratch', 'from_existing'));
ALTER TABLE missions ADD COLUMN repo_path TEXT;
ALTER TABLE missions ADD COLUMN main_branch TEXT NOT NULL DEFAULT 'main';
ALTER TABLE missions ADD COLUMN merge_strategy TEXT NOT NULL DEFAULT 'llm_resolve';
ALTER TABLE missions ADD COLUMN parent_mission_id TEXT REFERENCES missions(id) ON DELETE SET NULL;
ALTER TABLE missions ADD COLUMN use_incremental_worktree INTEGER NOT NULL DEFAULT 1;

-- Artifacts
CREATE TABLE IF NOT EXISTS artifacts (
    id TEXT PRIMARY KEY,                    -- "<task_id>.<local_name>"
    mission_id TEXT NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
    producer_task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    type TEXT NOT NULL CHECK (type IN ('design_doc','api_spec','schema','code_module','test_module','config','docs','report')),
    summary TEXT NOT NULL DEFAULT '',
    file_paths TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_artifacts_mission ON artifacts(mission_id);
CREATE INDEX IF NOT EXISTS idx_artifacts_producer ON artifacts(producer_task_id);

-- Task base conflicts (FR-07.1)
CREATE TABLE IF NOT EXISTS task_base_conflicts (
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    parent_task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    file_path TEXT NOT NULL,
    resolution TEXT NOT NULL CHECK (resolution IN ('auto','heuristic_theirs','llm_resolved','llm_failed_fallback')),
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (task_id, parent_task_id, file_path)
);

-- Merge records (FR-08.3)
CREATE TABLE IF NOT EXISTS merge_records (
    id TEXT PRIMARY KEY,
    mission_id TEXT NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
    source_branch TEXT NOT NULL,
    target_branch TEXT NOT NULL,
    strategy_attempted TEXT NOT NULL,
    final_strategy TEXT NOT NULL,
    conflicted_files TEXT NOT NULL DEFAULT '[]',
    llm_resolution_succeeded INTEGER,
    build_passed_after_llm INTEGER,
    fallback_reason TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_merge_records_mission ON merge_records(mission_id);
```

**Migration 015: planner agent loop**

```sql
CREATE TABLE IF NOT EXISTS planner_sessions (
    id TEXT PRIMARY KEY,
    mission_id TEXT REFERENCES missions(id) ON DELETE CASCADE,  -- nullable: mission 创建前先建 session
    kind TEXT NOT NULL DEFAULT 'planner'
        CHECK (kind IN ('planner', 'preflight')),  -- FR-19.5: pre-flight 共用 sessions/steps 表
    contract_id TEXT REFERENCES mission_contracts(id) ON DELETE SET NULL,  -- FR-20: contract-aware Planner
    repo_path TEXT NOT NULL,
    description TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'running'
        CHECK (status IN ('running', 'completed', 'failed', 'cancelled')),
    total_steps INTEGER NOT NULL DEFAULT 0,
    total_tokens INTEGER NOT NULL DEFAULT 0,
    error_message TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    completed_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_planner_sessions_mission ON planner_sessions(mission_id);

CREATE TABLE IF NOT EXISTS planner_steps (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES planner_sessions(id) ON DELETE CASCADE,
    step_no INTEGER NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('tool_call', 'tool_result', 'thinking', 'text')),
    tool_name TEXT,
    tool_args TEXT,        -- JSON
    tool_result TEXT,      -- JSON or string，超过 8KB 截断
    text_content TEXT,     -- 模型纯文本输出
    tokens_used INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_planner_steps_session ON planner_steps(session_id, step_no);

-- 一次 plan 内"允许此次"的临时白名单（plan 结束自动清除）
CREATE TABLE IF NOT EXISTS planner_session_fetch_grants (
    session_id TEXT NOT NULL REFERENCES planner_sessions(id) ON DELETE CASCADE,
    domain TEXT NOT NULL,
    PRIMARY KEY (session_id, domain)
);
```

**Migration 016: chat & follow-up**

```sql
CREATE TABLE IF NOT EXISTS mission_chats (
    id TEXT PRIMARY KEY,
    mission_id TEXT NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK (role IN ('user','assistant','system')),
    content TEXT NOT NULL,
    tool_calls TEXT,
    artifact_refs TEXT,
    proposed_followup_mission_id TEXT REFERENCES missions(id) ON DELETE SET NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_mission_chats_mission ON mission_chats(mission_id, created_at);
```

---

## AR — Architecture Requirements

### 数据模型 ER 概览

```text
┌─────────┐   1     N  ┌─────────┐  N        N  ┌──────────┐
│ Mission │───────────▶│  Task   │◀────────────▶│Dependency│
└─────────┘            └─────────┘              └──────────┘
     │                      │                         │
     │ 1                    │ 1     N                 │ artifact_refs[]
     │                      ▼                         │
     │                ┌──────────┐                    │
     │  artifacts[]   │ Artifact │◀───────────────────┘
     └───────────────▶└──────────┘
     │
     │ 1     N
     ▼
┌─────────────┐    1   N   ┌──────────────┐
│MissionChat  │            │ MergeRecord  │
└─────────────┘            └──────────────┘

(Role / Skill 不在 DB；启动时从 file system 加载到 in-memory registry)
```

### Planner 流水线（Agent Loop 版，含 Contract 整合）

```text
plan_mission(mission_id, repo_path, contract_id?)   [FR-22 / FR-20]
  │
  ├─ INSERT planner_sessions (kind='planner', contract_id=?, status='running')
  │
  ├─ build system prompt:
  │     PLANNER_SYSTEM_PROMPT_V2
  │     + 若 contract_id: append [Contract Hard Constraints]    [FR-20.2]
  │         scope[] + exclusions[]
  │
  ├─ AgentEngine::run_planner(
  │       system=<above>,
  │       tools=[                              [FR-05.2]
  │         list_directory, read_file, search_code, detect_tech_stack,
  │         list_roles, query_skills, get_skill_detail,
  │         propose_task, add_dependency, revise_task, drop_task,
  │         validate_plan, finalize_plan,
  │         fetch_url (受 FR-05.6 守卫),
  │         若 contract_id: get_contract, get_contract_section    [FR-20.3]
  │       ]
  │   )
  │
  │     loop with timeout / max_steps:        [FR-11 / FR-06.3]
  │       LLM call
  │       ├─ tool_use: dispatch
  │       │    ├─ propose_task → in-memory DAG accumulate + FR-04.1 校验
  │       │    │   ├─ pass → 返回 task_id
  │       │    │   └─ fail → 返回 error，LLM 修正后重试
  │       │    ├─ add_dependency / revise_task / drop_task → 同上
  │       │    ├─ validate_plan → 返回 issue 列表
  │       │    ├─ fetch_url → FR-05.6 白名单守卫
  │       │    │   ├─ 黑名单 → 拒绝
  │       │    │   ├─ 白名单 / 已授权 → 执行
  │       │    │   └─ 首次 → emit planner-fetch-confirmation
  │       │    │             阻塞等待 confirm_planner_fetch
  │       │    └─ finalize_plan → 跳出循环 → guardrail check
  │       │
  │       └─ 持久化 step → INSERT planner_steps + emit planner-step
  │
  ├─ guardrail_check
  │    ├─ DAG 一致性 (FR-09 / FR-04.1)                — 全 mode 启用
  │    ├─ 若 contract_id:                            — FR-21
  │    │    ScopeCoverage (LLM Judge, hard)
  │    │    ExclusionsUntouched (LLM Judge, hard)
  │    │    BudgetEstimate / DurationEstimate / QualityThreshold (soft)
  │    │
  │    ├─ 全 hard pass → persist DAG
  │    │    ├─ UPDATE missions (status='planned', title)
  │    │    ├─ INSERT tasks (全部新字段)
  │    │    ├─ INSERT artifacts (declarations only, file_paths 暂空)
  │    │    └─ INSERT task_dependencies (含 artifact_refs)
  │    │
  │    └─ 任一 hard fail：
  │         retry_count++ < budget → 注入错误 → 重入 loop
  │         else → planner_session.status=failed
  │
  ├─ UPDATE planner_sessions (completed)
  ├─ DELETE planner_session_fetch_grants WHERE session_id=...
  └─ emit planner-session-completed
```

### Runtime 流水线（增量 Worktree 版）

```text
Scheduler::poll_and_dispatch (每秒)
  │
  ├─ for each mission in active:
  │    slots = max_concurrent - count_running(mission_id)   [FR-13]
  │    ready = get_ready_tasks(mission_id)
  │
  └─ for each task in ready[..slots]:
       claim_task (ready → running)
       ▼
       prepare_task_base(task)                               [FR-07]
         ├─ 直接父分支拓扑后序
         ├─ git checkout -b task-base/<id> main_branch
         ├─ for parent in parents:
         │    git merge agent/<parent> -X theirs
         │    记录 task_base_conflicts (若有)
         └─ 触发 task-base-prepared 事件
       ▼
       create_worktree(task-base/<id>) → checkout -b agent/<id>
       ▼
       AgentEngine::run(task, worktree)
         ├─ build system_prompt
         │    ├─ Role base_prompt
         │    ├─ [Skills] (default + additional)             [FR-02.6]
         │    ├─ [Project Structure] + [Tech Stack]          [FR-10.1]
         │    ├─ [Upstream Context] + [Base Conflicts]       [FR-10.2]
         │    ├─ [Task]: title/description/expected_output
         │    │   /produces_artifacts/consumes_artifacts/file_hints
         │    └─ [Tools]
         │
         ├─ loop with timeout / max_steps:                   [FR-11]
         │    LLM call
         │    ├─ tool_use: execute (含 publish_artifact 校验) [FR-03.3]
         │    ├─ task_complete: 跳出循环 → guardrail check    [FR-09]
         │    └─ 无 tool_use 且无 task_complete:
         │         consecutive_no_tool++ → 注入催促
         │
         └─ guardrail_check
              ├─ 全 pass → completed + completion_summary
              ├─ 任一 fail:
              │    retry_count++
              │    if retry_count < budget:
              │      把失败信息 append 为 user message → 重入 loop
              │    else:
              │      task failed (但 commit 已修改文件)       [FR-11.4]
              └─ 触发 guardrail-result 事件
       ▼
       commit_worktree
       advance_dependencies                                  [FM-02]
       check_mission_terminal
         ├─ 全 completed → merge_completed_mission           [FR-07.5]
         │    └─ 仅合并 frontier task → main (用三层策略)     [FR-08]
         ├─ 有 failed 且无后续可推进 → mission failed
         └─ 仍有 ready/running → 继续轮询
       ▼
       (Mission completed)
       collect_delivery_payload → emit mission-delivered    [FR-14.1]
```

### 增量 Worktree 示意

```text
                                        ┌─ agent/T1 (T1 完成)
                                        │
main ●──●──●──● ────────────────────────┤
                                        │   ┌─ agent/T2 (基于 main+T1)
                                        │   │
                                        │   │      ┌─ agent/T4 (基于 main+T2+T3)
                                        │   │      │
                                        ▼   ▼      ▼
                                       ●─●─●─●────●───●──●  agent/T4
                                          │      ▲
                                          │      │
                                       agent/T1  ├─ task-base/T4 = merge(T2, T3)
                                                 │
                                                 └─ agent/T3 (基于 main+T1)

合并到 main 时，只需 merge T4 (frontier)，因为 T2/T3 已被 T4 包含
```

### 三层冲突合并状态机

```text
merge(branch, strategy)
  │
  ▼
git merge <branch>
  │
  ├─ no conflict → ✅ done
  │
  └─ has conflicts
       │
       ▼
       L2: 启发式
         ├─ 仅 import / whitespace 差异 → checkout --theirs → ✅ done
         └─ 实质冲突 → 进 L3 (若 strategy=llm_resolve)
                       │
                       ▼
                       L3: LLM 解冲突
                         ├─ 收集冲突上下文 → call LLM → 写回 → git add
                         ├─ build guardrail
                         │    ├─ pass → ✅ done (标记 llm_resolved)
                         │    └─ fail → 回退 L2 (-X theirs)
                         │              + fallback_reason 入库
                         └─ LLM 调用失败 → 回退 L2

(strategy=theirs / ours 跳过 L3，强制 -X theirs / -X ours)
```

### Guardrail 完成检测状态机

```text
LLM 响应
  │
  ├─ tool_use: task_complete
  │     │
  │     ▼
  │   for guardrail in task.guardrails:
  │     execute → emit guardrail-result
  │     │
  │     ├─ 全 pass → COMPLETED
  │     │
  │     └─ 任一 fail
  │          │
  │          ├─ retry_count++ < budget
  │          │     → append [Guardrail Failed] message → continue loop
  │          │
  │          └─ retry_count >= budget
  │                → FAILED (但 commit 已有改动)
  │
  ├─ tool_use: 其它工具
  │     execute → consecutive_no_tool=0 → continue loop
  │
  └─ 无 tool_use
       consecutive_no_tool++
       ├─ < 3 → 默默继续
       └─ >= 3 → 注入 "请使用工具或 task_complete" → 重置计数
                若再次触发 → 累计 max_steps 后超步失败
```

### Codebase Intelligence 注入流程

```text
AgentEngine::build_system_prompt(task)
  │
  ├─ section: ROLE
  │   role_template.base_prompt
  │
  ├─ section: SKILLS                                  [FR-02.6 / FR-10.4 优先级 1]
  │   for skill in task.role.default_skills + task.additional_skills:
  │     load_skill_body(skill)
  │     append "[Skill: name]\n<body>\n[/Skill]"
  │
  ├─ section: UPSTREAM CONTEXT                        [FR-10.2 / 优先级 2]
  │   if task.depends_on not empty:
  │     for parent in completed_parents:
  │       append summary + artifacts list
  │
  ├─ section: PROJECT STRUCTURE                       [FR-10.1 / 优先级 3]
  │   tree -L 3 (gitignore + 黑名单)
  │
  ├─ section: TECH STACK                              [FR-10.1 / 优先级 4]
  │   detect_tech_stack(workspace)
  │
  ├─ section: BASE CONFLICTS                          [FR-10.2 / 优先级 5]
  │   if task_base_conflicts not empty:
  │     append 冲突文件清单 + 解决方式说明
  │
  ├─ section: TASK
  │   title / description / expected_output
  │   produces_artifacts / consumes_artifacts
  │   file_scope_hints / guardrails
  │
  └─ truncate to 12000 tokens (按优先级保留)
```

### 后端模块变更

| 文件 | 变更 | 说明 |
|------|------|------|
| `agent/roles.rs` | 新增 | RoleTemplate 定义、加载、内置 6 角色 |
| `skills/registry.rs` | 新增 | SKILL.md 扫描、frontmatter 解析、按需加载 body |
| `skills/types.rs` | 新增 | SkillMeta, SkillBody, GuardrailDecl |
| `agent/planner.rs` | 重构 | PlannerTask schema 扩展、PROMPT_V2、Agent Loop 入口（取代 call_planner 单次调用） |
| `agent/planner_tools.rs` | 新增 | Planner 工具集（A 探索 / B 元数据 / C 构建 / D 校验 / E 终止 / fetch_url / contract 查询）；A 类被 Pre-flight Agent 复用 (FR-19.2) |
| `agent/planner_state.rs` | 新增 | 内存中 in-progress DAG 状态机，支持增量 propose/revise/validate |
| `agent/contract_guardrail.rs` | 新增 | ScopeCoverage / ExclusionsUntouched LLM Judge + 软警告估算 (FR-21) |
| `agent/preflight.rs` (FM-10 已有) | 修改 | from_existing 模式装载只读探索工具 (FR-19) |
| `commands/preflight.rs` | 修改 | sign_contract 重构为薄壳 (FR-22) |
| `agent/engine.rs` | 重构 | Codebase intelligence、guardrail 检测、task_complete 工具；抽出共享 loop 供 planner 复用 |
| `agent/scheduler.rs` | 重构 | 增量 worktree dispatch、frontier merge |
| `agent/guardrail.rs` | 新增 | Guardrail 执行器（artifacts/command/files/llm_judge） |
| `agent/codebase_intel.rs` | 新增 | tree/tech_stack/upstream_context 收集 |
| `agent/chat.rs` | 新增 | Follow-up Chat Agent + propose 工具 |
| `git/worktree.rs` | 重构 | prepare_task_base、frontier merge、main 检测 |
| `git/merge_strategy.rs` | 新增 | L1/L2/L3 三层合并实施 |
| `git/llm_resolver.rs` | 新增 | LLM 解冲突 prompt + build 校验 |
| `tools/definitions.rs` | 修改 | 新增 publish_artifact, task_complete, propose_followup_mission |
| `tools/executor.rs` | 修改 | 新工具的执行 + artifact 入库 |
| `db/migrations.rs` | 新增 | Migration 014 + 015 |
| `db/queries.rs` | 修改 | 大量新字段读写、artifact CRUD、merge_records、chat_messages |
| `commands/mission.rs` | 修改 | create_mission 接受 repo_origin/repo_path；plan_mission 必需 repo_path 且接受可选 contract_id；新 TaskInfo、artifact 查询 |
| `commands/agent.rs` | 修改 | start_mission_execution 启用增量 worktree |
| `commands/chat.rs` | 新增 | send_chat_message / get_chat_history / confirm_followup_mission |
| `commands/skills.rs` | 新增 | list_skills / get_role_templates |
| `commands/planner.rs` | 新增 | get_planner_steps / confirm_planner_fetch |
| `commands/config.rs` | 修改 | 新配置项 |
| `lib.rs` | 修改 | 注册新 command + 启动时初始化 SkillRegistry / RoleRegistry |

### 前端模块变更

| 文件 | 变更 | 说明 |
|------|------|------|
| `ipc/commands.ts` | 修改 | 包装新 command |
| `ipc/events.ts` | 修改 | 新增 7 类事件监听 |
| `stores/agent-store.ts` | 修改 | artifact / merge_record / chat 状态 |
| `stores/skill-store.ts` | 新增 | RoleTemplate + Skill 元数据 |
| `views/MissionsView.tsx` | 重构 | 接入 MissionDeliveryPanel、Chat 入口 |
| `components/mission/TaskDAG.tsx` | 重构 | role 颜色/图标、artifact 摘要 tooltip |
| `components/mission/dag-layout.ts` | 修改 | 边的中点 label 渲染支持 |
| `components/mission/TaskEditDialog.tsx` | 重构 | role / skill / artifact 编辑表单 |
| `components/mission/MissionDeliveryPanel.tsx` | 新增 | 终态交付面板 |
| `components/mission/FollowupChat.tsx` | 新增 | Chat UI + propose 弹窗 |
| `components/mission/ArtifactBadge.tsx` | 新增 | 边/节点上的 artifact 摘要徽标 |
| `components/mission/RoleBadge.tsx` | 新增 | 节点上的 role 标识 |

### 与其他模块交互

- **← FM-01**：复用 Mission/Task 基础数据模型，本 FM 在其上扩展字段
- **← FM-02**：复用 Scheduler 主循环、worktree 抽象、依赖推进；本 FM 在 dispatch_task 中插入 prepare_task_base、在 merge 中替换为 frontier merge + 三层策略
- **← FM-03**：复用 CancellationToken 机制
- **↔ FM-10 (Pre-flight Contract)**：本 FM 改动 Pre-flight：
  - `from_existing` 模式装载只读探索工具集 (FR-19)
  - `sign_contract` 重构为薄壳 (FR-22)
  - Contract 通过 `contract_id` 参数注入 Planner Loop (FR-20)
  - Planner 新增 ScopeCoverage / ExclusionsUntouched guardrail 校验 contract 满足 (FR-21)
- **→ FM-12 (Mission Report)**：completion_summary、artifacts、merge_records 是 Mission Report 的核心数据源
- **→ FM-13 (Harness Dashboard)**：guardrail 失败率、LLM 解冲突成功率、超时率等纳入 dashboard 指标
- **↔ FM-04 (Activity Stream)**：新事件（artifact-published、merge-decision、guardrail-result 等）需要在活动流可视化

### 现有代码关键入口（待改）

| 说明 | 文件路径 |
|------|---------|
| Planner（DAG 生成） | `src-tauri/src/agent/planner.rs` — `PLANNER_SYSTEM_PROMPT` (L546-569), `PlannerTask` (L578-584), `parse_and_validate` (L608) |
| Agent 引擎（执行循环） | `src-tauri/src/agent/engine.rs` — system prompt + 完成判定 |
| 调度器 | `src-tauri/src/agent/scheduler.rs` — `dispatch_task`, `merge_completed_mission` |
| Worktree | `src-tauri/src/git/worktree.rs` — `merge_agent_branch`（硬编码 `main`） |
| 工具定义 | `src-tauri/src/tools/definitions.rs` |
| 工具执行 | `src-tauri/src/tools/executor.rs` |
| DB queries | `src-tauri/src/db/queries.rs` |
| DB migrations | `src-tauri/src/db/migrations.rs` |
| 配置 | `src-tauri/src/commands/config.rs` |
| 前端 DAG | `src/components/mission/TaskDAG.tsx`, `dag-layout.ts` |
| 前端事件 | `src/ipc/events.ts` |

---

## 实施阶段拆分

### Phase 1: 数据模型 + Planner Agent Loop + Pre-flight 整合（Week 1-2）

**目标**：跑通两条创建路径——
- 直接 plan：「新建 mission（含 repo_origin） → Planner Agent Loop → DAG」
- 经 pre-flight：「新建 mission → Pre-flight 对话（已有 repo 模式可读代码）→ sign → 同一 Planner Loop（含 contract）→ DAG」

| 任务 | 涉及 FR | 验收 |
|------|---------|------|
| Migration 014 + 015 + 016 落地 | 数据需求 | `cargo test` 通过 |
| RoleTemplate 加载 + 内置 6 角色 | FR-01 | `get_role_templates` 命令返回 6 项 |
| Skill Registry + 内置 6 skill | FR-02 (1-7) | `list_skills` 返回内置 + 用户目录中的 skill |
| PlannerTask schema 扩展 | FR-04 | 单测：合法/非法 task 通过/拒绝 |
| Planner in-memory DAG state（propose/revise/validate） | FR-05.2-C/D | 单测：增量构造 + 一致性校验 |
| Planner 工具集 A/B 实施（探索 + 元数据） | FR-05.2-A/B, FR-05.5 | 各工具单测 |
| `fetch_url` 工具 + 黑白名单守卫 + 确认事件 | FR-05.6 | 集成测试：黑名单拒绝 / 白名单通过 / 首次确认流程 |
| AgentEngine 抽出共享 loop，新增 `run_planner` 入口 | FR-06.2 | Planner Loop 端到端 mock 测试 |
| Planner V2 PROMPT 编写 | FR-06.1 | 真实 LLM 调用：典型需求能跑通完整流程 |
| `planner_steps` 流式持久化 + 事件（kind 区分 planner/preflight） | FR-05.7, FR-19.5 | 前端能实时看到工具调用 |
| Planner Guardrail（DAG 一致性） | FR-04.1, FR-09 | 单测：缺 artifact / 环 / role 不存在等场景 |
| Artifact CRUD + publish_artifact 工具 | FR-03 | 单元测试覆盖 |
| **Mission 创建表单：repo_origin + repo_path** | FR-18 | 两种 origin 路径手测：from_scratch 自动 mkdir+git init；from_existing 校验 |
| **Pre-flight 装载只读探索工具（仅 from_existing）** | FR-19 | 集成测试：Pre-flight Agent 能 read_file 并基于代码提问 |
| **`get_contract` / `get_contract_section` 工具** | FR-05.2-B (扩展), FR-20.3 | 工具单测 |
| **Planner Contract 双层注入 + system prompt 拼装** | FR-20 | snapshot 测试：有/无 contract 两种 prompt |
| **ScopeCoverage / ExclusionsUntouched LLM Judge** | FR-21.3 | 集成测试：构造覆盖/未覆盖两种 DAG，guardrail 正确判定 |
| **软警告 guardrail（Budget / Duration / Quality）** | FR-21.4 | 单测 |
| **`sign_contract` 重构为薄壳** | FR-22 | 端到端：Pre-flight → sign_contract → DAG（与直接 plan 走同一 Loop） |
| 前端 PlannerSessionView（取代 PlannerStreamPanel） | FR-17 | 步骤式渲染：每步显示 tool_call + result |
| 前端 fetch_url 确认弹窗 | FR-05.6, FR-17 | 三种决策路径手测 |
| 前端 Pre-flight 步骤面板（同步用 PlannerSessionView，按 kind 区分样式） | FR-17, FR-19.5 | Pre-flight 探索可视化 |
| 前端 RoleBadge + ArtifactBadge | FR-16 (1-2) | DAG 可视化呈现新字段 |

**Phase 1 完成标准**：以下两条路径均端到端跑通：
1. **直接 plan**：新建 mission（from_existing 一个真实 Rust 项目）→ 直接 plan_mission → Planner 探索代码 → DAG UI 呈现 role/artifact
2. **Pre-flight 路径**：新建 mission → Pre-flight 对话（Agent 能基于代码问出有针对性的问题，如「我看到你已用 sqlx，新功能是否沿用？」）→ 构建 contract → sign → 同一 Planner Loop 接收 contract → 通过 ScopeCoverage guardrail → DAG 含全部 scope item

### Phase 2: Runtime + 增量 Worktree（Week 2）

**目标**：替换 dispatch / merge 流程，跑通增量 worktree。

| 任务 | 涉及 FR | 验收 |
|------|---------|------|
| `prepare_task_base` 实施 | FR-07.1 | 单测：菱形 DAG 能正确合并直接父 |
| 主分支自动检测 | FR-12 | 非 main 默认分支仓库可启动 |
| Mission 级并发隔离 | FR-13 | 多 mission 不互抢配额 |
| `dispatch_task` 接入增量 worktree | FR-07.3 | 端到端：菱形 DAG 中 D 启动时 worktree 已含 B+C |
| `merge_completed_mission` 改为 frontier merge | FR-07.5 | 端到端：菱形 DAG 完成后 main 包含全部产出 |
| L1 + L2 启发式合并 | FR-08.1, FR-08.2(1-2) | 单测覆盖 |
| `task_base_conflicts` + `merge_records` 入库 | FR-07.1, FR-08.3 | 查询命令返回正确数据 |

**Phase 2 完成标准**：菱形 DAG 端到端跑通，下游 worktree 含上游内容；冲突按 L1+L2 处理。

### Phase 3: 完成检测 + LLM 解冲突 + Codebase Intel（Week 3）

**目标**：替换完成判定，开启 LLM 解冲突，注入 codebase intelligence。

| 任务 | 涉及 FR | 验收 |
|------|---------|------|
| `task_complete` 工具 + Guardrail 执行器 | FR-09 (1-6) | Guardrail 全 pass / 部分 fail / 重试预算耗尽三种路径单测 |
| 三种内置 Guardrail（artifacts/command/files） | FR-09.6 | 各自单测 |
| LLM Judge Guardrail | FR-09.6 | 集成测试（mock LLM） |
| Codebase intelligence 注入 | FR-10 | snapshot 测试 system prompt 输出 |
| Agent 超时 + 步数限制 | FR-11 | 模拟超时单测 |
| L3 LLM 解冲突 + build 校验 | FR-08.2(3), FR-08.4 | 集成测试：构造冲突场景 |
| 前端 events 封装补全 | FR-17 | 事件可在 store 中观测 |

**Phase 3 完成标准**：扇出/扇入 DAG 含真实文件冲突场景能端到端跑通，含至少一次 LLM 解冲突；Guardrail 失败能正确重试。

### Phase 4: 终态交付 + Follow-up Chat（Week 4）

**目标**：补全交付面板与 Chat 闭环。

| 任务 | 涉及 FR | 验收 |
|------|---------|------|
| `mission-delivered` 事件 + payload 收集 | FR-14.1 | 单测：完成的 mission 触发事件并含全部字段 |
| `MissionDeliveryPanel` 组件 | FR-14.2 | 视觉验收 |
| `Open in Editor` / `Open in Terminal` | FR-14.3 | macOS / Linux / Windows 各验证一次 |
| Chat Agent 实施 | FR-15.1-3, 15.5-6 | 单测 + 端到端：小改动直接 commit |
| `propose_followup_mission` + 确认弹窗 | FR-15.4 | 端到端：大改动触发 propose 并能升级为子 mission |
| Chat UI + 流式输出 | FR-15.6, FR-17 | 视觉验收 |
| 文档（用户手册中"Skills"和"Roles"章节） | — | docs/ 提交 |

**Phase 4 完成标准**：Mission 完成 → 看到交付面板 → chat 输入小修改即做 → chat 输入大需求弹窗确认 → 走 plan 流程产生子 mission → 子 mission 端到端完成 → 全链路无人值守可工作。

---

## 风险与开放问题

| 风险 | 影响 | 缓解 |
|------|------|------|
| LLM 解冲突质量不稳定 | mission 合并可能引入逻辑错误 | 强制 build guardrail；UI 标红待复核；提供回退到 L2 的开关 |
| 增量 worktree 在大 DAG 上性能下降 | 每 task 多 N 次 git merge | NFR-03 限定 ≤ 5s；超 N 个父时记录 warn |
| Skill 体内容过大撑爆 prompt | Agent 上下文溢出 | FR-10.4 上限 + 优先级截断；建议 skill ≤ 8KB |
| Planner 生成的 artifact ID 不稳定 | 重新规划时 ID 漂移 | 将 ID 设计为内容确定性派生（task_id 已稳定，local_name 强制 snake_case）|
| Chat agent 误判改动规模 | 大改动直接 commit 污染 main | FR-15.5 行数硬阈值兜底 |
| Role 闭枚举限制 LLM 表达 | 某些任务无合适 role | role_templates.json 允许用户增补；预留 `general` 兜底 role（v2 加入）|

## 兼容性

- **配置兼容**：旧配置缺失新字段 → 用默认值
- **DB 兼容**：所有 ALTER TABLE 都带 DEFAULT，旧 mission/task 可继续工作
- **行为兼容**：旧 mission（`use_incremental_worktree=0`）走旧合并逻辑；通过 `MissionsView` 重启历史 mission 时弹窗确认是否升级
- **前端兼容**：旧 TaskInfo 缺新字段时 UI 渲染降级（不显示 role badge / artifact label）
