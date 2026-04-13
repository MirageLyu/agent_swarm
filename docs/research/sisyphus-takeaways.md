# Sisyphus 编排器调研 — 可落地改进

> 日期: 2026-04-10
> 来源: oh-my-claude-sisyphus (oh-my-claudecode) 多 Agent 编排框架
> 对照: Miragenty FM-02 / FM-03 现有实现

---

## 背景

Sisyphus 是 oh-my-opencode 生态中的多 Agent 编排器，采用"资深工程师式"动态委派模式，
与 Miragenty 的"静态 DAG + 调度器"模式互补。以下两个设计点与现有架构兼容度高，
实施成本低，值得引入。

---

## TODO 1: Task 完成验证机制

**来源**: Sisyphus 的"西西弗斯必须把石头推到山顶"原则 — Agent 声称完成后，
强制执行验证流水线才能真正标记为 completed。

### 现状

当前 `complete_task` 的逻辑链:

```
Agent 无工具调用 → AgentStatus::Completed
  → commit_worktree (git add -A + commit)
  → complete_task (status → completed)
  → advance_dependencies (下游 pending → ready)
```

**问题**: Agent 的"完成"完全依赖 LLM 自身判断（不再发起工具调用），
没有任何客观验证。Agent 可能提交了编译不通过、lint 报错、测试失败的代码，
但只要它不再调用工具，就会被标记为 completed 并触发下游任务。

### 改进方案

在 `commit_worktree` 之后、`complete_task` 之前，插入一个验证阶段:

```
Agent 完成 → commit_worktree
  → [NEW] 验证阶段:
     1. 在 worktree 中执行 build (如存在 package.json / Cargo.toml)
     2. 在 worktree 中执行 lint (如存在 .eslintrc / clippy 配置)
     3. 在 worktree 中执行 test (如存在测试脚本)
  → 全部通过 → complete_task → advance_dependencies
  → 任一失败 → 反馈给 Agent 继续修复 (不消耗 max_steps)
```

### 设计要点

- **可配置**: 验证项目列表应可配置（Mission 级或全局），而非硬编码
- **宽容模式**: 首次迭代可仅 warn 不 block，避免误判导致 Agent 死循环
- **与 FM-11 的关系**: FM-11 Evaluator Agent 是 Agent 间的深度代码审查，
  本提案是更轻量的"编译/lint/测试通过"门禁，二者互补不冲突:
  - 验证机制 = CI 管线（客观、机械、快速）
  - FM-11 Evaluator = Code Review（主观、智能、深度）
- **关联代码**:
  - `src-tauri/src/agent/engine.rs` — `AgentStatus::Completed` 判定处
  - `src-tauri/src/agent/scheduler.rs` — `dispatch_task` / task 完成回调
  - `src-tauri/src/git/worktree.rs` — `commit_worktree` 后的时机点

### 优先级评估

**中高** — 直接提升产出可靠性。尤其在 DAG 中，上游任务的错误代码会
传播到所有下游（它们在 merge 后才能看到上游改动），越早拦截价值越大。

---

## TODO 2: 智能模型路由

**来源**: Sisyphus 按任务类型自动选择 Haiku / Sonnet / Opus 模型，
简单搜索任务用便宜模型，复杂实现用强模型。

### 现状

当前所有 Worker Agent 使用同一个 LLM provider 和模型:

```rust
// engine.rs
let response = self.llm_provider.stream_chat(messages, tools).await;
```

模型由全局 `LlmConfig` 决定，不区分任务类型。一个"搜索代码库中的
XXX 用法"任务和一个"实现完整的认证模块"任务使用同一个模型，
前者浪费了高端模型的 token，后者可能受限于低端模型的能力。

### 改进方案

在 Planner 输出的任务定义中增加复杂度标签，由调度器据此路由模型:

```
Planner DAG JSON:
  task: { id: "t1", complexity: "low",  ... }   → 路由到 Haiku/fast
  task: { id: "t2", complexity: "high", ... }   → 路由到 Opus/strong

Scheduler dispatch_task:
  match task.complexity {
    Low    → LlmProvider::new(haiku_config),
    Medium → LlmProvider::new(sonnet_config),
    High   → LlmProvider::new(opus_config),
  }
```

### 设计要点

- **Planner 侧**: 在 planner prompt 中要求为每个 task 标注
  `complexity: "low" | "medium" | "high"`，或使用更细粒度的 tag
  （如 `type: "research" | "implementation" | "refactor"`）
- **Scheduler 侧**: `dispatch_task` 读取 complexity，从预配置的
  model map 中选择对应的 LlmConfig
- **用户可覆盖**: Mission 级别或 Task 级别允许用户手动指定模型
- **成本节约估算**: 假设一个 Mission 有 5 个 task，其中 2 个是搜索/调研型，
  使用 Haiku 替代 Sonnet 可节省约 40-60% 的 token 成本
  （Haiku 定价约为 Sonnet 的 1/10）
- **关联代码**:
  - `src-tauri/src/agent/planner.rs` — Planner prompt / JSON schema 定义
  - `src-tauri/src/agent/scheduler.rs` — `dispatch_task` 中构造 `AgentEngine`
  - `src-tauri/src/llm/` — LlmConfig / Provider 抽象

### 优先级评估

**中** — 对多任务 Mission 有显著成本优化效果。但需要先验证 Planner
对 complexity 标注的准确度，以及低端模型在工具调用场景下的可靠性。
建议在 FM-11 Evaluator 就绪后回归验证不同模型的产出质量差异。

---

## TODO 3: DAG 边语义升级 — 从时序约束到上下文传递

**来源**: 对 Miragenty DAG vs LangGraph / Airflow 数据流模型的根本性对比。

### 现状

当前 DAG 的边 `A → B` 仅表示**纯时序约束**（"B 在 A 之后启动"），
边上不传递任何数据。具体表现：

```
A 完成 → commit 到 agent/A 分支
       → advance_dependencies → B 变为 ready
       → dispatch_task(B) → create_worktree(B, base = 主仓 HEAD)
```

B 的 worktree 从主仓 HEAD 创建，**看不到 A 的任何改动**。
A 的代码只存在于 `agent/A` 分支，要等到 Mission 全部完成后的
`merge_completed_mission` 才合入 HEAD。

### 问题

这使得 DAG 的依赖边成为一个**空承诺**：

1. **下游盲区**: 如果 Task A 创建了数据库 schema，Task B 要写
   使用该 schema 的 API，B 的 Agent 看不到 A 创建的文件，
   只能靠 task description 的自然语言"想象" A 做了什么
2. **纠错成本后移**: 上游 Agent 的错误（类型定义不匹配、接口签名不一致）
   只在最终 merge 时才暴露，此时所有下游 Agent 的工作可能都需要返工
3. **DAG 越深成本越高**: 尾部节点累积了所有上游的不确定性，
   拓扑序 merge 时冲突和不一致集中爆发

本质上，当前的 DAG 更接近 **Make/Bazel 的构建依赖模型**
（仅保证顺序，节点通过文件系统隐式通信），但 worktree 隔离
恰好切断了文件系统这条隐式通道。

### 演进方向（三选一，待评估）

**方案 A — 链式 Worktree（改动小，隔离性降低）**

```
dispatch_task(B) 时:
  如果 B 有上游依赖 [A]:
    base = agent/A 分支的最新 commit
  如果 B 有多个上游 [A, C]:
    先将 A + C merge 到临时分支 → base = 该临时分支
```

优点：B 物理上能看到 A 的全部文件。
缺点：多上游时需要预合并，引入冲突处理复杂度；
破坏了"所有 worktree 基线一致"的简洁模型。

**方案 B — 产出摘要注入（改动中等，隔离性不变）**

```
A 完成 → 生成 diff summary + 关键产出描述（文件列表、新增接口签名等）
       → dispatch_task(B, upstream_context = A 的产出摘要)
       → 注入到 B 的 system prompt 或首条 user message
```

优点：不破坏 worktree 隔离，复用现有 Note/Directive 注入机制。
缺点：摘要可能丢失细节；token 开销随上游数量增长。
**与现有架构兼容度最高。**

**方案 C — 共享状态层（改动大，最强表达力）**

```
引入 Mission 级 SharedState (JSON/SQLite key-value)
A 完成 → 写入 state["schema"] = "CREATE TABLE users ..."
B 启动 → 读取 state["schema"]
```

优点：最接近 LangGraph 的表达力，节点间可传递结构化数据。
缺点：引入共享可变状态，需要并发控制；与"Agent 独立执行"哲学冲突。

### 推荐

**先落地方案 B**（产出摘要注入），因为：
- 不需要改动 worktree / merge 模型
- 可复用 `format_notes_for_injection` 现有机制
- 摘要生成可以是确定性的（diff stat + 文件列表），不需要额外 LLM 调用
- 效果可直接观测（下游 Agent 的产出是否与上游一致性更好）

后续如果摘要不够用，再考虑方案 A 或 C。

### 关联代码

| 文件 | 改动点 |
|------|--------|
| `src-tauri/src/agent/scheduler.rs` | `dispatch_task` — 构造上游 context 注入 |
| `src-tauri/src/git/worktree.rs` | `commit_worktree` — 返回 diff summary |
| `src-tauri/src/agent/engine.rs` | 首轮 message 拼装 — 注入上游摘要 |
| `src-tauri/src/db/queries.rs` | `advance_dependencies` — 查询已完成上游的产出 |

### 优先级评估

**中高** — 这是 DAG 编排从"能用"到"好用"的关键一步。
随着用户构建更复杂的 Mission（5+ 任务、3+ 层深度），
上下游信息断裂的问题会越来越明显。建议在 FM-11 Evaluator 之前
或同步推进，因为 Evaluator 也需要理解上下游关系才能准确评审。

---

## 参考资料

- [oh-my-claude-sisyphus (GitHub)](https://github.com/Yeachan-Heo/oh-my-claudecode)
- [Sisyphus Orchestrator 文档](https://lzw.me/docs/opencodedocs/code-yeongyu/oh-my-opencode/start/sisyphus-orchestrator/)
- [Superset — 并行 Agent IDE](https://superset.sh/)
- 对比分析见本项目 Chat 记录
