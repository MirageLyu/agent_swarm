# FM-05: Code Review & Diff

> 版本: v1.0 | 日期: 2026-04-01  
> 优先级: P2 | 预估周期: 5-7 天  
> 依赖: FM-02 | 被依赖: 无

---

## IR — Initial Requirements

### IR-01: 用户故事

**US-01**: 作为开发者，我希望能在统一界面里查看各 Agent 的代码 diff，而不是自己去 worktree 翻文件。

**US-02**: 作为开发者，我希望能针对单个 Agent 的变更做 approve 或 request revision，这样评审链路完整。

### IR-02: 业务价值

- 把 worktree 结果转化为真正可评审的产出
- 为后续 Evaluator 审查、人类审批队列、批量通过做基础

### IR-03: 高层验收标准

1. 每个 Agent 的 diff 可读取和展示
2. 支持文件级、hunk 级浏览
3. 支持 approve/reject/revision 三种动作的最小闭环

---

## SR — Software Requirements

### 功能需求

#### FR-01: Diff 数据获取

- **FR-01.1**: 后端新增 `get_agent_diff(agent_id)` command
- **FR-01.2**: 从对应 worktree 获取 patch/diff 文本
- **FR-01.3**: 若无变更，返回空 diff 状态

#### FR-02: Diff Review UI

- **FR-02.1**: 新增 Review 视图，可按 Mission 聚合显示 Agent diff
- **FR-02.2**: 使用 Monaco Diff Editor 展示文件差异
- **FR-02.3**: 左侧文件树列出变更文件，右侧展示当前文件 diff
- **FR-02.4**: 支持切换不同 Agent 的 diff

#### FR-03: 审批动作

- **FR-03.1**: 提供 `Approve`, `Request Revision`, `Reject` 三种动作
- **FR-03.2**: `Request Revision` 需填写反馈文本
- **FR-03.3**: 审批动作和反馈文本保存到数据库（`agent_events` 表，`kind = 'review'`），**Phase 1 仅记录和展示，不自动触发 Agent 重新执行**。自动重执行是 Phase 2 Evaluator 闭环功能的范畴
- **FR-03.4**: 审批结果在 UI 中可见（Agent 卡片上显示 approved/rejected/revision_requested 状态）

### 非功能需求

- **NFR-01**: 10 个变更文件以内切换流畅
- **NFR-02**: 大 diff 文件需懒加载或分页策略预留

### 接口需求

| Command | 参数 | 返回 | 说明 |
|---------|------|------|------|
| `get_agent_diff` | `{ agent_id }` | `{ files[] }` | 获取结构化 diff |
| `submit_review_action` | `{ agent_id, action, comment? }` | `()` | 提交审查动作 |

---

## AR — Architecture Requirements

### 前端组件设计

| 组件 | 路径 | 职责 |
|------|------|------|
| `ReviewView` | `src/views/ReviewView.tsx` | Review 容器页 |
| `AgentReviewTabs` | `src/components/review/AgentReviewTabs.tsx` | 切换 Agent |
| `DiffFileTree` | `src/components/review/DiffFileTree.tsx` | 文件树 |
| `DiffViewer` | `src/components/review/DiffViewer.tsx` | Monaco Diff Editor |
| `ReviewActionBar` | `src/components/review/ReviewActionBar.tsx` | 审批动作栏 |

### 后端模块变更

| 文件 | 变更 |
|------|------|
| `git/worktree.rs` | 扩展结构化 diff 读取 |
| `commands/agent.rs` 或 `commands/review.rs`（新） | review 相关命令 |

### 与其他模块交互

- **← FM-02**: 获取 `worktree_path`
- **→ FM-06**: revision comment 可转成运行时注入内容

### 现有代码与依赖说明

**git diff 已有基础**：`src-tauri/src/git/worktree.rs` 的 `get_diff(agent_id)` 已能返回 patch 文本。本模块需要将其扩展为结构化输出。

**Monaco Editor 需新安装**：

```bash
pnpm add @monaco-editor/react monaco-editor
```

**`get_agent_diff` 返回结构建议**：

```typescript
interface DiffFile {
  path: string;
  status: "added" | "modified" | "deleted";
  old_content: string | null;
  new_content: string | null;
}
interface AgentDiffResponse {
  agent_id: string;
  files: DiffFile[];
}
```

**ReviewView 需要注册为新的 sidebar 导航项**：在 `Sidebar.tsx` 和 `useUiStore` 中新增 `review` 视图。

**审批结果存储**：建议新增 `agent_reviews` 表或复用 `agent_events` 的 `kind` 扩展。
