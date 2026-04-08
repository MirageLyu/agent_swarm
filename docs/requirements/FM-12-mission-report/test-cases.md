# FM-12: Mission Report — 测试用例

> 版本: v1.0 | 日期: 2026-04-08

---

## 单元测试 (UT)

### UT-01: 报告数据汇聚（Rust）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|---------|
| UT-01.1 | 完整 Mission 数据 | 3 tasks, 2 agents, 1 evaluator review | 报告 JSON 包含所有节数据 |
| UT-01.2 | 无 Evaluator 数据 | mission 无 evaluator_reviews | Evaluator 节为空数组，不报错 |
| UT-01.3 | 无 Contract | mission 无 contract | Contract 对照节为 null |
| UT-01.4 | 成本汇总 | 3 agents 各有 cost_records | By Model 和 By Task 分组正确 |
| UT-01.5 | 时长计算 | mission created_at 到 completed_at | duration 秒数正确 |

### UT-02: LLM 摘要生成（Rust）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|---------|
| UT-02.1 | 正常生成 | 汇聚数据 JSON | 返回 1-2 段文字摘要 |
| UT-02.2 | LLM 超时 | 30 秒超时 | 使用降级摘要（模板拼接） |
| UT-02.3 | 决策提取 | agent_events 含 tool_use 选择 | 提取出 2-5 个 Decision |

### UT-03: 投票逻辑（Rust）

| ID | 场景 | 操作 | 期望结果 |
|----|------|------|---------|
| UT-03.1 | 首次投票 | vote_decision(agree) | report_votes 插入 1 行 |
| UT-03.2 | 重复投票 | 同一 decision 再次投票 | 更新而非新增（UNIQUE 约束） |
| UT-03.3 | 切换投票 | agree → disagree | vote 字段更新 |

### UT-04: Markdown 导出（Rust）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|---------|
| UT-04.1 | 正常导出 | 完整报告数据 | 生成 .md 文件，含标题/表格/列表 |
| UT-04.2 | 特殊字符 | 报告含 `|` 和 `` ` `` 字符 | Markdown 转义正确 |
| UT-04.3 | 文件写入失败 | 路径不可写 | 返回明确错误 |

### UT-05: ReportTOC 组件（TS）

| ID | 场景 | 操作 | 期望结果 |
|----|------|------|---------|
| UT-05.1 | 渲染 7 个锚点 | 完整报告 | 7 个 TOC 链接 |
| UT-05.2 | Scrollspy | 滚动到 Cost 节 | "Cost Breakdown" 项高亮 |
| UT-05.3 | 点击导航 | 点击 "Task Matrix" | 正文平滑滚动到对应节 |

### UT-06: DecisionCard 组件（TS）

| ID | 场景 | 操作 | 期望结果 |
|----|------|------|---------|
| UT-06.1 | 渲染决策 | Decision D-1 数据 | 显示标题、Rationale、Trade-off、Risk |
| UT-06.2 | 投票 Agree | 点击 Agree | 按钮选中态、计数 +1 |
| UT-06.3 | 投票 Disagree | 点击 Disagree | Agree 取消、Disagree 选中 |
| UT-06.4 | 已投票态 | 加载时有 vote | 对应按钮为选中态 |

---

## 集成测试 (IT)

### IT-01: 报告生成端到端

| 步骤 | 操作 | 期望结果 |
|------|------|---------|
| 1 | 完成一个含 3 task 的 Mission | Mission status=completed |
| 2 | 导航到 Report 视图 | 报告开始生成（loading） |
| 3 | 生成完成（≤30s） | 显示完整报告，7 个节均有内容 |
| 4 | 滚动浏览 | TOC 高亮跟随 |
| 5 | 折叠/展开节 | 动画流畅 |

### IT-02: Contract 对照

| 步骤 | 操作 | 期望结果 |
|------|------|---------|
| 1 | 通过 Pre-flight 完成 Mission | 有 Contract |
| 2 | 打开报告 → 打开 "Compare with Contract" | 右侧面板显示 Contract 条目 |
| 3 | 检查达成状态 | 已完成条目绿色、未达成条目红色 |

### IT-03: 投票与持久化

| 步骤 | 操作 | 期望结果 |
|------|------|---------|
| 1 | 打开报告 → Architecture Decisions 节 | 显示 2-3 个决策卡片 |
| 2 | 对 D-1 投 Agree | 按钮选中、计数显示 |
| 3 | 关闭应用重新打开 | 投票状态保留 |

### IT-04: Markdown 导出

| 步骤 | 操作 | 期望结果 |
|------|------|---------|
| 1 | 打开报告 → 点击 "Export Markdown" | 文件选择对话框弹出 |
| 2 | 选择保存路径 | 文件写入成功，toast 提示 |
| 3 | 打开导出文件 | 内容完整、格式正确 |

---

## 边界测试 (BT)

| ID | 场景 | 操作 | 期望结果 |
|----|------|------|---------|
| BT-01 | Failed Mission 报告 | Mission failed | 报告正常生成、Summary 反映失败原因 |
| BT-02 | 无 Agent Events | mission 无执行历史 | 决策节为空，不报错 |
| BT-03 | 报告重复生成 | 同一 mission 多次调用 | 覆盖旧报告或提示已存在 |
| BT-04 | 极大 Mission | 30 tasks, 10 agents | 报告生成成功、表格可滚动 |
| BT-05 | LLM 生成摘要失败 | API 不可用 | 使用降级模板、不阻塞报告展示 |
| BT-06 | 导出到只读路径 | 选择 /System 目录 | 显示权限错误提示 |
