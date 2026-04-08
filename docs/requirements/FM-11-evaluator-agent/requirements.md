# FM-11: Evaluator Agent & Quality Scoring

> 版本: v1.0 | 日期: 2026-04-08  
> 优先级: P0 | 预估周期: 7-10 天  
> 依赖: FM-02（多 Agent 调度）、FM-05（Code Review 基础）、FM-10（Contract 提供验收标准） | 被依赖: FM-12, FM-13  
> 原型参考: `design/prototypes/05-code-review.html`（Evaluator 注释/评分部分）

---

## IR — Initial Requirements

### IR-01: 用户故事

**US-01**: 作为开发者，我希望每个 Agent 完成任务后有一个独立的 Evaluator Agent 自动审查其产出，这样我不需要逐行人工审查。

**US-02**: 作为开发者，我希望 Evaluator 能对代码产出给出行级注释（类似 GitHub PR review），并按类别（bug/style/performance/security/suggestion）分类标记，这样我能快速定位重点。

**US-03**: 作为开发者，我希望 Evaluator 能给出每个文件和每个 Agent 的质量评分（0-10），这样我能量化产出质量。

**US-04**: 作为开发者，我希望 Evaluator 发现的部分问题可以自动修复（Auto-fix），减少我的审查负担。

### IR-02: 业务价值

- **核心差异化**：自动代码审查 + 质量评分是产品的关键卖点
- **Review Reduction Rate**：量化用户审查工作量的降低，是产品核心指标
- **反馈闭环**：Evaluator 结果回流到 Learning Flywheel，持续改善 Agent 行为
- **Contract 闭环**：验收标准对照实际产出，自动评估 Contract 达成度

### IR-03: 高层验收标准

1. Agent 完成任务后自动触发 Evaluator Agent 审查
2. Evaluator 生成行级注释，插入 Code Review Diff 视图中
3. 每个注释有类别标签和严重度
4. 每个文件显示质量评分 x/10
5. 支持"Auto-fix"——Evaluator 修复简单问题后标注"已自动修复"
6. 支持用户对注释执行 "Request Revision" 或 "Dismiss"
7. Review 摘要栏显示 Evaluator 发现的总问题数/已修复/待审

---

## SR — Software Requirements

### 功能需求

#### FR-01: Evaluator Agent 引擎

- **FR-01.1**: 新增 `EvaluatorAgent` 模块，复用 `agent/engine.rs` 的步进循环，但 system prompt 和工具集不同
- **FR-01.2**: Evaluator 的 system prompt 接收：Agent 产出的 diff、Contract 验收标准（如有）、项目上下文
- **FR-01.3**: Evaluator LLM 输出结构化 JSON：
  ```json
  {
    "file_reviews": [
      {
        "file_path": "src/auth/handler.rs",
        "score": 8.5,
        "annotations": [
          {
            "line": 42,
            "type": "security",
            "severity": "warning",
            "message": "JWT expiration is hardcoded to 24h",
            "suggestion": "Use environment variable JWT_EXPIRES_IN",
            "auto_fixable": false
          }
        ]
      }
    ],
    "overall_score": 8.0,
    "summary": "..."
  }
  ```
- **FR-01.4**: 支持 Auto-fix：Evaluator 标记 `auto_fixable: true` 的注释，自动在 Agent 的 worktree 中应用修改
- **FR-01.5**: Auto-fix 后创建一个新 commit，commit message 标注 `[evaluator-auto-fix]`

#### FR-02: 评审触发与调度

- **FR-02.1**: Agent 完成任务（status=completed）后，Scheduler 自动创建 Evaluator Agent 实例
- **FR-02.2**: Evaluator Agent 使用同一个 worktree（只读 diff 访问 + auto-fix 写入权限）
- **FR-02.3**: Evaluator 完成后将结果写入 `evaluator_reviews` 表
- **FR-02.4**: 如果 Evaluator 评分低于 Contract quality_threshold，自动标记任务为 `needs_revision`

#### FR-03: 注释数据模型

- **FR-03.1**: `evaluator_annotations` 表存储行级注释，关联到 agent_id + file_path + line_number
- **FR-03.2**: 注释类型枚举：`bug`, `style`, `performance`, `security`, `suggestion`
- **FR-03.3**: 注释严重度枚举：`error`, `warning`, `info`
- **FR-03.4**: 注释状态枚举：`open`, `auto_fixed`, `revision_requested`, `dismissed`

#### FR-04: Review UI 集成

- **FR-04.1**: Code Review Diff 视图中，Evaluator 注释作为内联注释行插入到对应代码行下方
- **FR-04.2**: 注释样式按严重度区分：error=红底左边线、warning=橙底、info/suggestion=绿底
- **FR-04.3**: 每条注释显示：Evaluator Agent 标识 + 类别标签 + 消息内容 + 操作按钮
- **FR-04.4**: Auto-fixed 注释显示"已自动修复"绿色标签 + "View Original"按钮
- **FR-04.5**: Needs-review 注释显示"需人工审核"橙色标签 + "Request Revision" / "Dismiss" 按钮
- **FR-04.6**: 文件 header 区域显示 `Score: x/10`

#### FR-05: Review 摘要与交互

- **FR-05.1**: ReviewView 顶部汇总栏显示："Evaluator 发现 N 个问题：X 已自动修复 · Y 需人工审核"
- **FR-05.2**: "Request Revision"：标记注释状态为 `revision_requested`，通知对应 Agent 重新处理（Phase 2 MVP 仅记录状态，不自动触发）
- **FR-05.3**: "Dismiss"：注释折叠淡出，状态变为 `dismissed`
- **FR-05.4**: "View Original"：toast 提示或展开修复前的代码行

### 非功能需求

- **NFR-01**: Evaluator 审查单个 Agent 产出 ≤ 30 秒
- **NFR-02**: 注释渲染不影响 Diff 视图滚动性能（100 条注释内）
- **NFR-03**: Auto-fix 的 commit 在 worktree 中不产生冲突（如有冲突则跳过该 fix）

### 接口需求

新增 Tauri Commands：

| Command | 参数 | 返回 | 说明 |
|---------|------|------|------|
| `trigger_evaluation` | `{ agent_id }` | `{ evaluator_agent_id }` | 手动触发 Evaluator |
| `get_evaluation_result` | `{ agent_id }` | `EvaluationResult` | 获取评审结果 |
| `get_annotations` | `{ agent_id, file_path? }` | `Annotation[]` | 获取注释列表 |
| `update_annotation_status` | `{ annotation_id, status }` | `()` | 更新注释状态 |

新增 Tauri Events：

| Event | Payload | 说明 |
|-------|---------|------|
| `evaluation-complete` | `{ agent_id, overall_score, annotation_count }` | Evaluator 完成通知 |

### 数据需求

新增 Schema 迁移：

```sql
CREATE TABLE IF NOT EXISTS evaluator_reviews (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    mission_id TEXT NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
    overall_score REAL NOT NULL DEFAULT 0.0,
    summary TEXT NOT NULL DEFAULT '',
    contract_compliance TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS evaluator_annotations (
    id TEXT PRIMARY KEY,
    review_id TEXT NOT NULL REFERENCES evaluator_reviews(id) ON DELETE CASCADE,
    agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    file_path TEXT NOT NULL,
    line_number INTEGER NOT NULL,
    type TEXT NOT NULL
        CHECK (type IN ('bug', 'style', 'performance', 'security', 'suggestion')),
    severity TEXT NOT NULL DEFAULT 'info'
        CHECK (severity IN ('error', 'warning', 'info')),
    status TEXT NOT NULL DEFAULT 'open'
        CHECK (status IN ('open', 'auto_fixed', 'revision_requested', 'dismissed')),
    message TEXT NOT NULL,
    suggestion TEXT,
    auto_fixable INTEGER NOT NULL DEFAULT 0,
    original_code TEXT,
    fixed_code TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

---

## AR — Architecture Requirements

### 前端组件设计

| 组件 | 路径 | 职责 |
|------|------|------|
| `EvaluatorAnnotation` | `src/components/review/EvaluatorAnnotation.tsx` | 内联注释行 |
| `AnnotationTag` | `src/components/review/AnnotationTag.tsx` | 类别/状态标签 |
| `FileScore` | `src/components/review/FileScore.tsx` | 文件级评分显示 |
| `EvalSummaryBar` | `src/components/review/EvalSummaryBar.tsx` | Evaluator 摘要栏 |

### 后端模块变更

| 文件 | 变更 |
|------|------|
| `agent/evaluator.rs`（新） | Evaluator Agent 引擎：system prompt、结果解析、auto-fix 逻辑 |
| `agent/scheduler.rs` | Agent completed → 自动创建 Evaluator |
| `commands/review.rs` | 新增 evaluation 相关 commands |
| `db/migrations.rs` | 新增 evaluator_reviews、evaluator_annotations 表 |

### 与其他模块的交互

- **← FM-02**: Scheduler 在 Agent 完成后触发 Evaluator
- **← FM-05**: 注释插入到 Code Review Diff 视图
- **← FM-10**: Contract 验收标准作为 Evaluator 评判依据
- **→ FM-12**: 评审结果纳入 Mission Report
- **→ FM-13**: 质量评分统计纳入 Harness Dashboard

### 现有代码关键入口

| 说明 | 文件路径 |
|------|---------|
| Agent 引擎步进循环 | `src-tauri/src/agent/engine.rs` |
| Agent 调度器 | `src-tauri/src/agent/scheduler.rs` |
| Code Review 组件 | `src/views/ReviewView.tsx`, `src/components/review/` |
| Git worktree diff | `src-tauri/src/git/worktree.rs` |
| LLM Provider | `src-tauri/src/llm/` |
