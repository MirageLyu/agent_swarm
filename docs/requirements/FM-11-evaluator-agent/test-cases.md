# FM-11: Evaluator Agent & Quality Scoring — 测试用例

> 版本: v1.0 | 日期: 2026-04-08

---

## 单元测试 (UT)

### UT-01: Evaluator JSON 解析（Rust）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|---------|
| UT-01.1 | 合法评审结果 | 2 文件、各 1 注释、overall_score=8.5 | 解析成功 |
| UT-01.2 | 空注释 | file_reviews 有文件但无 annotations | 解析成功，score 有效 |
| UT-01.3 | 非法 type | type="unknown" | 返回解析错误 |
| UT-01.4 | 缺失字段 | annotation 缺少 line | 返回 MissingField 错误 |
| UT-01.5 | 评分越界 | score=15 | 返回 InvalidScore 错误（0-10 范围） |

### UT-02: Auto-fix 逻辑（Rust）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|---------|
| UT-02.1 | 可修复注释 | auto_fixable=true + fixed_code | 在 worktree 中应用修改 |
| UT-02.2 | 修复冲突 | 目标行已被修改 | 跳过修复、注释标记为 open |
| UT-02.3 | Auto-fix commit | 2 个 auto-fix | 创建 1 个 commit，message 含 [evaluator-auto-fix] |

### UT-03: 评审触发调度（Rust）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|---------|
| UT-03.1 | Agent completed | agent status → completed | 自动创建 Evaluator Agent |
| UT-03.2 | Agent failed | agent status → failed | 不触发 Evaluator |
| UT-03.3 | Evaluator 评分低于阈值 | score=5, threshold=7 | task 标记为 needs_revision |
| UT-03.4 | 无 Contract | mission 无 contract | Evaluator 仍运行（无 compliance 检查） |

### UT-04: 注释 CRUD（Rust）

| ID | 场景 | 操作 | 期望结果 |
|----|------|------|---------|
| UT-04.1 | 写入注释 | 批量插入 5 条 | 全部可查询 |
| UT-04.2 | 更新状态 | dismissed | status=dismissed |
| UT-04.3 | 按文件过滤 | file_path=src/auth.rs | 仅返回该文件注释 |
| UT-04.4 | 按 Agent 过滤 | agent_id=xxx | 仅返回该 Agent 注释 |

### UT-05: EvaluatorAnnotation 组件（TS）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|---------|
| UT-05.1 | 渲染 error 注释 | severity=error | 红底 + 红左边线 |
| UT-05.2 | 渲染 warning 注释 | severity=warning | 橙底 + 橙左边线 |
| UT-05.3 | 渲染 auto-fixed | status=auto_fixed | 绿色"已自动修复"标签 |
| UT-05.4 | 渲染 needs-review | status=open, auto_fixable=false | 橙色"需人工审核"标签 |
| UT-05.5 | Dismiss 交互 | 点击 Dismiss | 注释淡出、调用 update_annotation_status |
| UT-05.6 | Request Revision | 点击按钮 | 按钮变"Requested"、调用后端 |

### UT-06: FileScore 组件（TS）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|---------|
| UT-06.1 | 高分 | score=9.5 | 绿色显示 "9.5/10" |
| UT-06.2 | 中分 | score=6.5 | 橙色显示 "6.5/10" |
| UT-06.3 | 低分 | score=3.0 | 红色显示 "3.0/10" |

---

## 集成测试 (IT)

### IT-01: Evaluator 自动触发端到端

| 步骤 | 操作 | 期望结果 |
|------|------|---------|
| 1 | 创建 Mission → 确认 → Agent 开始执行 | Agent 状态 running |
| 2 | Agent 完成任务 | agent status → completed |
| 3 | 观察 Evaluator 触发 | 自动创建 Evaluator Agent |
| 4 | Evaluator 完成 | evaluation-complete 事件推送 |
| 5 | 打开 ReviewView | 注释插入 Diff 行间、文件显示 Score |

### IT-02: Auto-fix 流程

| 步骤 | 操作 | 期望结果 |
|------|------|---------|
| 1 | Agent 产出含安全问题的代码 | 写入 worktree |
| 2 | Evaluator 审查 | 标记 auto_fixable=true |
| 3 | Auto-fix 执行 | worktree 中文件修改 + 新 commit |
| 4 | ReviewView 查看 | 注释显示"已自动修复" + "View Original" |

### IT-03: Review 摘要与操作

| 步骤 | 操作 | 期望结果 |
|------|------|---------|
| 1 | 打开含 Evaluator 注释的 ReviewView | 顶部汇总栏显示问题数 |
| 2 | Dismiss 一条注释 | 注释淡出、汇总更新 |
| 3 | Request Revision 一条注释 | 状态变为 revision_requested |

### IT-04: Contract 合规性检查

| 步骤 | 操作 | 期望结果 |
|------|------|---------|
| 1 | 通过 Pre-flight 签署 Contract（quality_threshold=8） | Contract 已签 |
| 2 | Agent 执行并完成 | Evaluator 自动触发 |
| 3 | Evaluator 评分 6.5 | 任务标记为 needs_revision |
| 4 | 评审结果中含 contract_compliance 字段 | 列出未达标验收标准 |

---

## 边界测试 (BT)

| ID | 场景 | 操作 | 期望结果 |
|----|------|------|---------|
| BT-01 | LLM 返回无效评审格式 | Evaluator LLM 回复纯文本 | 重试一次，失败标记评审错误 |
| BT-02 | 大量注释 | 单文件 100+ 注释 | Diff 视图正常渲染、可滚动 |
| BT-03 | 无 diff | Agent 未修改任何文件 | Evaluator 跳过，score=10 |
| BT-04 | Auto-fix 冲突 | 目标行已被删除 | 跳过该 fix、注释保留 open |
| BT-05 | Evaluator 超时 | 审查时间 > 30s | 超时终止、记录部分结果 |
| BT-06 | 并发 Evaluator | 3 个 Agent 同时完成 | 3 个 Evaluator 并行运行 |
| BT-07 | 手动触发 + 自动触发 | 用户手动 trigger 后 Agent 完成 | 不重复创建 Evaluator |
