# 需求分析说明书: Coding Agent 测评系统

**功能分支**: `001-coding-agent-eval`  
**创建日期**: 2026-05-22  
**状态**: 草稿  
**输入**: 用户描述: "为 Miragenty 的 Coding Agent 接入高复杂度、对标业界标准的测评系统；重点研究 GA-Technical-Report 中 Task Completion & Token Efficiency 与 Tool-Use Efficiency 两个维度，并规划如何接入本项目。参考仓库：https://github.com/JinyiHan99/GA-Technical-Report.git"

---

## 1. 简介

### 1.1 目的

本文档定义 Miragenty 中 Coding Agent 能力测评系统的需求。该系统用于持续、可复现、可对比地评估单个 Coding Agent 的任务完成质量、token 使用效率、工具使用效率、请求效率和执行稳定性，为后续单 Agent 能力强化提供量化反馈闭环。

### 1.2 范围

包含：

- 面向 Coding Agent 的 benchmark 数据集接入、运行、评分、指标聚合和结果对比。
- 首批对齐 GA-Technical-Report 的两个维度：
  - Task Completion & Token Efficiency。
  - Tool-Use Efficiency。
- 支持导入 GA-Technical-Report 仓库中的数据集格式，包括 SOP-Bench、Lifelong AgentBench、RealFin-Benchmark 和 Tool Efficiency Benchmark。
- 支持自动 grader、混合 grader、人工/LLM judge 预留。
- 支持记录完整执行轨迹、token、LLM request、tool call、tool error、runtime、workspace artifacts、final response、grader 输出。
- 支持后续将同一评测基础设施推广到 Planner Agent、Evaluator Agent、Follow-up Chat Agent 等其他 Agent。

不包含：

- 本阶段不直接优化 Coding Agent 策略本身。
- 本阶段不要求提供完整公开排行榜。
- 本阶段不要求复现 GA 报告中的所有 baseline 结果。
- 本阶段不要求把外部 benchmark 资产提交进主仓；应支持用户本地导入或配置路径。

### 1.3 假设和约束

| 类型 | 描述 |
|------|------|
| 假设 | Coding Agent 的执行轨迹可由现有 `agent_events`、`cost_records`、`agents`、`tasks` 表重建。 |
| 假设 | GA-Technical-Report 仓库提供 benchmark 任务定义、输入资产和 grader，但不提供历史运行轨迹、token 统计或工具调用轨迹。 |
| 假设 | 初期评测目标是 Miragenty 自身 Coding Agent，不要求直接驱动 Claude Code、OpenAI Codex、OpenClaw 等外部 agent。 |
| 约束 | benchmark 执行必须隔离 workspace，不能污染用户真实仓库或当前 Miragenty 源码树。 |
| 约束 | 涉及外部网络、shell、文件写入、潜在高成本 LLM 调用时，必须纳入可配置的权限和预算控制。 |
| 约束 | Python grader 执行存在安全风险，必须在受控目录和有限输入输出约束下运行。 |

### 1.4 术语与缩写

| 术语/缩写 | 全称 | 说明 |
|-----------|------|------|
| Coding Agent | Miragenty Coding Agent | 基于 `AgentEngine` 执行单个 task 的 Agent。 |
| Benchmark Suite | Benchmark Suite | 一组可批量运行的 benchmark 样本集合。 |
| Benchmark Case | Benchmark Case | 单个评测样本，包含 prompt、资产、预期输出和 grader。 |
| TSR | Task Success Rate | 任务成功率，成功样本数 / 总样本数。 |
| Token Efficiency | Token Efficiency | 达成任务成功所消耗的 input/output token、总 token 或单位成功 token 成本。 |
| Tool-Use Efficiency | Tool-Use Efficiency | 达成任务成功所需工具调用数、工具错误率、工具选择冗余、LLM 请求数等效率指标。 |
| Grader | Grader | 对 agent 输出和 workspace 产物进行自动评分的程序。 |
| Run | Benchmark Run | 一次 benchmark suite 执行记录。 |
| Sample Result | Sample Result | 一次 benchmark case 的执行和评分结果。 |

---

## 2. 系统上下文

### 2.1 系统定位

该功能位于 Miragenty 的 Agent 质量闭环层，服务于“单 Agent 能力强化”。它不是普通测试套件，而是面向 LLM Agent 行为的评测系统，覆盖成功率、效率、成本、轨迹质量和可复现性。它应作为后续优化 Agent prompt、工具集、上下文压缩、恢复机制、guardrail 和 role-specific 行为的量化依据。

### 2.2 系统边界

系统内部交互：

- 读取 benchmark suite/case 定义。
- 创建隔离 workspace。
- 通过 Coding Agent 执行 case prompt。
- 采集 Agent 执行事件、成本、状态、产物。
- 调用 grader 计算任务成功和细分评分。
- 聚合 run 级指标并提供查询/展示接口。

外部边界：

- 可导入本地 GA-Technical-Report 数据集目录。
- 可执行本地 Python grader。
- 可调用用户已配置的 LLM provider。
- 可在部分 benchmark 中访问网络，但必须受策略控制。

### 2.3 系统架构图

```text
Benchmark Dataset / GA Repo
        |
        v
Benchmark Importer ----> Benchmark Registry / DB
        |
        v
Benchmark Runner ----> Isolated Workspace ----> Coding Agent / AgentEngine
        |                                           |
        |                                           v
        |                                  agent_events / cost_records / artifacts
        v
Grader Executor ----> Sample Score ----> Run Aggregator ----> UI / Export / Comparison
```

---

## 3. 需求分析概述

### 3.1 需求来源

- 用户要求集中提升 Coding Agent 能力，并先为其接入高复杂度、业界标准级测评系统。
- GA-Technical-Report 提供评测维度参考：Task Completion & Token Efficiency、Tool-Use Efficiency。
- 当前 Miragenty 已有 Agent 执行事件、成本记录、工具调用和任务状态基础设施，适合接入 benchmark runner。

### 3.2 需求目标

- 让 Coding Agent 的能力提升从主观观察转为可量化评测。
- 能批量运行 benchmark，并保存可复现实验记录。
- 能回答“是否完成任务”“花了多少 token”“用了多少 LLM 请求”“用了多少工具调用”“工具使用是否冗余/错误”“相同任务在不同模型/配置/版本下是否变好”。
- 能支撑后续所有 Agent 共用的评测基础设施。

### 3.3 关键利益相关者

| 角色 | 职责 | 关注点 |
|------|------|--------|
| Miragenty 产品/研发负责人 | 决定 Agent 能力优化方向 | 指标是否可信、是否能发现真实短板、是否能横向对比版本 |
| Agent 能力开发者 | 优化 Coding Agent 行为 | 失败轨迹、token 浪费点、工具误用点、回归检测 |
| 测试/发布负责人 | 发布前验证 Agent 能力 | benchmark 可复现、阈值可配置、报告可导出 |
| 高级用户 | 使用评测结果选择模型/配置 | 成功率、成本、速度、稳定性 |

---

## 4. 需求场景分析

### 4.1 业务场景

#### 4.1.1 业务流程图

```text
选择 benchmark suite
        |
配置模型 / Agent 参数 / 预算 / 并发
        |
启动 benchmark run
        |
逐 case 创建 workspace 并运行 Coding Agent
        |
采集轨迹与执行成本
        |
运行 grader / judge
        |
聚合 Task Completion、Token Efficiency、Tool-Use Efficiency
        |
展示趋势、失败样本、可导出报告
```

#### 4.1.2 业务规则

| 规则编号 | 规则名称 | 规则描述 | 优先级 |
|----------|----------|----------|--------|
| BR-001 | 可复现运行 | 每次 run 必须记录模型、provider、Agent 配置、代码版本、benchmark 版本、case hash、开始/结束时间。 | 高 |
| BR-002 | 隔离执行 | 每个 case 必须在独立 workspace 中运行，case 间不得共享未声明状态。 | 高 |
| BR-003 | 指标一等公民 | 成功率、token、请求数、工具调用数、工具错误数、runtime 必须作为结构化字段保存。 | 高 |
| BR-004 | 原始轨迹保留 | grader 之外必须保留 agent event trace，支持失败复盘。 | 高 |
| BR-005 | 批量可中断 | benchmark run 可停止，已完成 sample result 不丢失，未完成样本标记为 cancelled/failed。 | 中 |

### 4.2 用户操作场景与关键路径

#### 4.2.1 用户角色定义

| 角色ID | 角色名称 | 角色描述 | 权限范围 |
|--------|----------|----------|----------|
| UR-001 | Agent 能力开发者 | 运行 benchmark 并分析结果 | 创建 run、查看详情、导出报告 |
| UR-002 | 发布负责人 | 使用固定 suite 做回归门禁 | 启动预设 run、查看通过/失败阈值 |
| UR-003 | 高级用户 | 比较模型或配置效果 | 查看摘要和历史趋势 |

#### 4.2.2 用户操作场景

**场景 US-001: 导入 GA Tool Efficiency Benchmark 并运行 Coding Agent** (优先级: P1)

| 项目 | 描述 |
|------|------|
| 场景描述 | 用户选择本地 GA-Technical-Report 目录，系统识别 Tool Efficiency Benchmark 的 16 个样本并创建 suite，然后运行 Coding Agent。 |
| 前置条件 | 已配置 LLM provider；本地存在 GA 数据集目录；允许创建临时 workspace。 |
| 操作步骤 | 1. 选择导入路径。<br>2. 系统解析 JSONL、assets、graders。<br>3. 用户选择全部或部分 case。<br>4. 启动 run。<br>5. 系统逐 case 执行并评分。 |
| 预期结果 | run 完成后展示 TSR、平均 token、平均请求数、平均工具调用数、工具错误率、每 case grader 明细。 |
| 异常处理 | 数据集缺失、grader 失败、Agent 超时、预算耗尽均以结构化状态记录。 |

**场景 US-002: 评估 Task Completion & Token Efficiency** (优先级: P1)

| 项目 | 描述 |
|------|------|
| 场景描述 | 用户运行 SOP-Bench、Lifelong AgentBench 或 RealFin-Benchmark，评估任务完成质量和 token 成本。 |
| 前置条件 | benchmark case 有可执行 grader 或评分规则。 |
| 操作步骤 | 1. 选择 suite。<br>2. 配置模型和 Agent 参数。<br>3. 运行 benchmark。<br>4. 查看成功率与 token 分布。 |
| 预期结果 | 系统能计算 case success、suite TSR、总 token、成功样本平均 token、失败样本平均 token、单位成功 token 成本。 |
| 异常处理 | 无参考答案或 judge 不可用时，case 标记为 ungraded，不计入 TSR 分母或按配置计入失败。 |

**场景 US-003: 对比两次 Agent 配置的工具效率** (优先级: P1)

| 项目 | 描述 |
|------|------|
| 场景描述 | 用户比较两次 run，判断新版本是否减少工具调用、LLM request 和 token，同时保持任务成功率。 |
| 前置条件 | 至少存在两个同 suite 的 run。 |
| 操作步骤 | 1. 选择 baseline run 和 candidate run。<br>2. 系统按 case 对齐。<br>3. 展示指标 delta。 |
| 预期结果 | 用户能看到成功率变化、token 变化、工具调用变化、请求数变化、runtime 变化和失败样本变化。 |
| 异常处理 | suite/case 不一致时，系统只比较共同 case 并提示覆盖率。 |

#### 4.2.3 关键路径分析

```text
Dataset Import -> Run Creation -> Case Workspace Setup -> Agent Execution -> Trace Collection -> Grading -> Aggregation -> Comparison
```

| 路径编号 | 路径描述 | 关键节点 | 风险点 |
|----------|----------|----------|--------|
| KP-001 | 单 case 评测闭环 | workspace、Agent、events、grader、result | final response 捕获不完整、grader 不可信、工具事件缺字段 |
| KP-002 | 批量 run 聚合 | run 状态、case 状态、metric aggregation | 中断恢复、并发隔离、预算控制 |
| KP-003 | 历史对比 | 版本元数据、case hash、metric schema | benchmark 版本漂移、模型参数不一致 |

### 4.3 研发与维护阶段场景

#### 4.3.1 开发阶段场景

| 场景 | 描述 | 注意事项 |
|------|------|----------|
| 新增 benchmark adapter | 为新的 JSONL/CSV/自定义 suite 添加 importer | adapter 输出必须归一化为统一 case schema |
| 新增指标 | 添加如 context efficiency、recovery rate、patch quality 等指标 | 指标必须能追溯到原始事件或 grader 输出 |
| 扩展到其他 Agent | Planner/Evaluator/Chat 复用 runner 和 metric 存储 | Agent 类型差异通过 adapter 层隔离 |

#### 4.3.2 测试阶段场景

| 场景 | 描述 | 测试要点 |
|------|------|----------|
| dry-run 测试 | 使用 mock provider 或极小 case 验证 runner | 不调用真实 LLM 也能验证 DB 和 grader 流程 |
| grader 测试 | 对固定 workspace/response 验证 grader 输出 | grader stdout 必须可解析，失败可诊断 |
| 回归测试 | 比较同一 suite 的历史 run | case hash、metric 计算稳定 |

#### 4.3.3 运维阶段场景

| 场景 | 描述 | 运维要求 |
|------|------|----------|
| 长跑 benchmark | suite 可能运行数小时并产生较高成本 | 支持停止、恢复、预算、进度展示 |
| 数据清理 | 大量 workspace 和 trace 会占用磁盘 | 支持保留策略和显式清理 |
| 报告归档 | 导出 JSON/Markdown/CSV | 导出包含复现实验元数据 |

### 4.4 兼容性分析

#### 4.4.1 版本兼容性

| 兼容类型 | 兼容要求 | 影响范围 |
|----------|----------|----------|
| 向前兼容 | 指标 schema 应允许新增字段，不破坏旧 run 查询。 | DB、前端展示、导出格式 |
| 向后兼容 | 现有 Mission/Coding Agent 执行流程不应因 benchmark 功能改变。 | AgentEngine、Scheduler |

#### 4.4.2 平台兼容性

| 平台 | 兼容性要求 | 备注 |
|------|------------|------|
| macOS | 支持本地 benchmark、Python grader、Tauri UI | 当前主要开发环境 |
| Windows | 路径处理和 Python 执行需兼容 | 注意 shell 命令差异 |
| Linux | 支持 CI 或无头 benchmark runner | 可作为后续发布门禁基础 |

#### 4.4.3 数据兼容性

| 数据类型 | 兼容性要求 | 迁移策略 |
|----------|------------|----------|
| benchmark case | 支持 JSONL/CSV/内置 manifest | 归一化入库并保存原始 JSON |
| event trace | 兼容现有 `agent_events.meta` | 通过聚合器重建指标 |
| result metrics | 支持新增指标和版本化 | JSON metrics + 关键列冗余 |

---

## 5. 功能性需求分析

### 5.1 功能需求列表

| 需求编号 | 需求名称 | 需求描述 | 优先级 | 来源 |
|----------|----------|----------|--------|------|
| FR-001 | Benchmark Suite 导入 | 系统可从本地目录导入 GA 风格 benchmark 数据集并归一化为 suite/case。 | P1 | 用户需求、GA 研究 |
| FR-002 | Coding Agent Benchmark Runner | 系统可批量运行 Coding Agent 处理 benchmark case。 | P1 | 用户需求 |
| FR-003 | Task Completion 评分 | 系统可通过 grader/judge 判定 case 是否成功，并计算 suite TSR。 | P1 | GA 4.1/4.2 |
| FR-004 | Token Efficiency 指标 | 系统可记录并聚合 input/output/total token、成本和单位成功 token。 | P1 | GA 4.1 |
| FR-005 | Tool-Use Efficiency 指标 | 系统可记录并聚合工具调用数、工具错误数、工具类别、请求数、runtime 等。 | P1 | GA 4.2 |
| FR-006 | Grader 执行与结果存储 | 系统可执行自动 grader，解析输出并保存细分评分。 | P1 | GA 数据集 |
| FR-007 | Run 对比 | 系统可比较同 suite 的不同 run，展示 per-case 与 aggregate delta。 | P1 | 能力优化闭环 |
| FR-008 | 可复现实验元数据 | 每次 run 必须记录模型、provider、Agent 配置、代码版本、benchmark 版本和 case hash。 | P1 | 业界评测标准 |
| FR-009 | UI/API 查询 | 系统提供 benchmark suite、run、sample result、metric summary 的查询能力。 | P2 | 产品集成 |
| FR-010 | Agent 类型扩展 | 测评系统的核心数据模型和 runner 抽象应支持未来扩展到其他 Agent。 | P2 | 用户后续规划 |
| FR-011 | 安全与预算控制 | benchmark 执行必须支持预算、超时、网络/命令权限和 workspace 隔离策略。 | P1 | 安全要求 |
| FR-012 | 导出报告 | 系统可导出 run 的 JSON/Markdown/CSV 报告。 | P2 | 研发协作 |

### 5.2 功能需求详细说明

#### FR-001: Benchmark Suite 导入

| 项目 | 描述 |
|------|------|
| 需求描述 | 用户可选择本地 benchmark 目录，系统识别支持的数据集并导入 suite/case。首批支持 GA-Technical-Report 中 `tool_efficiency_benchmark`、`sop_bench`、`lifelong_agentbench`、`realfin_benchmark`。 |
| 验收标准 | 1. 能识别 Tool Efficiency Benchmark 的 16 个 case。<br>2. 能保存 case prompt、assets、expected_outputs、grader、task_type、source_suite、target capability。<br>3. 能保存原始记录和 case hash。 |
| 关联场景 | US-001 |
| 依赖需求 | 无 |

#### FR-002: Coding Agent Benchmark Runner

| 项目 | 描述 |
|------|------|
| 需求描述 | 系统可为每个 case 创建隔离 workspace，复制 assets，构造 task prompt，运行 Coding Agent，并跟踪生命周期。 |
| 验收标准 | 1. 每个 case 有独立 sample result。<br>2. workspace 不污染源 benchmark 目录和用户项目。<br>3. Agent 状态 completed/failed/cancelled/timeout 被结构化记录。<br>4. 支持顺序执行和后续并发扩展。 |
| 关联场景 | US-001, US-002 |
| 依赖需求 | FR-001 |

#### FR-003: Task Completion 评分

| 项目 | 描述 |
|------|------|
| 需求描述 | 系统根据 grader 输出、预期文件、final response 或 LLM judge 结果计算 case 成功状态。 |
| 验收标准 | 1. 自动 grader 中的 `task_success` 可映射为 case success。<br>2. 无 grader 时可标记 ungraded 或使用 judge 策略。<br>3. suite TSR 可按全部样本和已评分样本分别计算。 |
| 关联场景 | US-001, US-002 |
| 依赖需求 | FR-006 |

#### FR-004: Token Efficiency 指标

| 项目 | 描述 |
|------|------|
| 需求描述 | 系统基于 `cost_records` 和 Agent 状态计算 token 使用效率。 |
| 验收标准 | 1. 记录 input_tokens、output_tokens、total_tokens、cost_usd。<br>2. 聚合 total、mean、median、p95、success-only mean、failure-only mean。<br>3. 计算 token_per_success = total_tokens / successful_cases。<br>4. 支持按模型、suite、case type 过滤。 |
| 关联场景 | US-002, US-003 |
| 依赖需求 | FR-002 |

#### FR-005: Tool-Use Efficiency 指标

| 项目 | 描述 |
|------|------|
| 需求描述 | 系统基于 `agent_events` 中 `tool_use`、`tool_result`、`llm_call`、`error` 等事件计算工具效率。 |
| 验收标准 | 1. 记录 llm_request_count。<br>2. 记录 tool_call_count、tool_result_count、tool_error_count。<br>3. 记录 tool_call_count_by_name。<br>4. 计算 tool_calls_per_success、requests_per_success、tool_error_rate、read_only_loop_hint_count、guardrail_retry_count、recovery_attempt_count。<br>5. 支持按 target capability 展示。 |
| 关联场景 | US-001, US-003 |
| 依赖需求 | FR-002 |

#### FR-006: Grader 执行与结果存储

| 项目 | 描述 |
|------|------|
| 需求描述 | 系统可在 case 完成后运行 grader，传入 workspace 和 final response 文件，解析 JSON 输出。 |
| 验收标准 | 1. Grader stdout JSON 被保存为原始结果。<br>2. Grader 退出码、stderr、duration 被保存。<br>3. Grader 失败不会丢失 Agent 执行结果。<br>4. Python grader 在受控工作目录中执行。 |
| 关联场景 | US-001, US-002 |
| 依赖需求 | FR-002 |

#### FR-007: Run 对比

| 项目 | 描述 |
|------|------|
| 需求描述 | 用户可选择两个同 suite run 进行横向对比。 |
| 验收标准 | 1. 展示 aggregate delta。<br>2. 展示 per-case success/token/tool/request/runtime delta。<br>3. 标出 regression、improvement、unchanged。 |
| 关联场景 | US-003 |
| 依赖需求 | FR-003, FR-004, FR-005 |

#### FR-008: 可复现实验元数据

| 项目 | 描述 |
|------|------|
| 需求描述 | 每次 run 保存完整复现条件。 |
| 验收标准 | 1. 保存 app git commit、dirty flag、model、provider、base_url hash、Agent 配置、benchmark source path、case hash。<br>2. 保存 run-level seed/并发/超时/预算策略。<br>3. 导出报告包含这些元数据。 |
| 关联场景 | US-002, US-003 |
| 依赖需求 | FR-002 |

#### FR-009: UI/API 查询

| 项目 | 描述 |
|------|------|
| 需求描述 | 前端或后续 CLI 可查询 suite、run、sample result 和 metric summary。 |
| 验收标准 | 1. 能列出 benchmark suites。<br>2. 能查看 run 列表和状态。<br>3. 能查看 sample result 详情，包括 trace 摘要和 grader 输出。<br>4. 能查看聚合指标。 |
| 关联场景 | US-001, US-003 |
| 依赖需求 | FR-001 至 FR-006 |

#### FR-010: Agent 类型扩展

| 项目 | 描述 |
|------|------|
| 需求描述 | 评测系统不应把数据模型锁死在 Coding Agent；应预留 agent_kind 和 adapter 能力。 |
| 验收标准 | 1. run 或 sample result 记录 agent_kind。<br>2. metric schema 支持不同 Agent 的公共指标和特有指标。<br>3. Coding Agent runner 是第一个 adapter，而非唯一实现。 |
| 关联场景 | 研发扩展场景 |
| 依赖需求 | FR-002 |

#### FR-011: 安全与预算控制

| 项目 | 描述 |
|------|------|
| 需求描述 | benchmark 运行必须可控，避免无限成本、污染文件系统或执行不可信 grader。 |
| 验收标准 | 1. 支持 run/case timeout。<br>2. 支持最大 token/cost 预算。<br>3. 支持禁用网络或仅允许显式授权网络。<br>4. 支持清理 workspace。<br>5. grader 权限边界有明确记录。 |
| 关联场景 | US-001, 运维场景 |
| 依赖需求 | FR-002, FR-006 |

#### FR-012: 导出报告

| 项目 | 描述 |
|------|------|
| 需求描述 | 用户可导出 benchmark run 报告供归档和讨论。 |
| 验收标准 | 1. JSON 导出包含全部结构化指标。<br>2. Markdown 导出包含摘要、配置、指标表、失败 case。<br>3. CSV 导出包含 per-case 指标。 |
| 关联场景 | US-003 |
| 依赖需求 | FR-003 至 FR-008 |

### 5.3 功能依赖关系

```text
FR-001 -> FR-002 -> FR-006 -> FR-003
              |         |
              v         v
          FR-004     FR-005
              \       /
               v     v
                FR-007 -> FR-012
FR-008 applies to all run/result records
FR-011 applies to runner/grader execution
FR-010 constrains architecture across runner/metrics
```

| 需求ID | 依赖需求 | 依赖类型 | 说明 |
|--------|----------|----------|------|
| FR-002 | FR-001 | 强依赖 | Runner 需要已归一化 case。 |
| FR-003 | FR-006 | 强依赖 | 成功判定依赖 grader/judge 输出。 |
| FR-004 | FR-002 | 强依赖 | token 指标依赖 Agent 执行记录。 |
| FR-005 | FR-002 | 强依赖 | 工具指标依赖 Agent 事件记录。 |
| FR-007 | FR-003, FR-004, FR-005 | 强依赖 | 对比依赖成功率和效率指标。 |
| FR-012 | FR-007, FR-008 | 弱依赖 | 报告需要指标和复现元数据。 |

---

## 6. 非功能性需求分析

### 6.1 系统定位

该系统定位为 Miragenty 的 Agent Evaluation Harness，目标用户是 Agent 能力开发者、发布负责人和高级用户。它应达到“可作为优化决策依据”的可信度，而不是仅作为演示功能。

### 6.2 系统可靠性

| 指标 | 要求 | 说明 |
|------|------|------|
| 数据持久性 | 已完成 sample result 不因 run 中断丢失 | 支持长跑 benchmark |
| 失败隔离 | 单 case 失败不影响其他 case | 除非用户选择 fail-fast |
| 可恢复性 | app 重启后可查看历史 run，后续可支持恢复 | 初期至少持久化状态 |
| 计量一致性 | 同一 trace 重算指标结果一致 | 指标计算应确定性 |

### 6.3 安全隐私

#### 6.3.1 安全需求

| 安全类型 | 需求描述 | 安全等级 |
|----------|----------|----------|
| 文件隔离 | benchmark workspace 必须位于受控目录，不写用户真实仓库。 | 高 |
| 命令执行 | grader 和 agent shell 均需受超时、工作目录和权限策略限制。 | 高 |
| 网络访问 | web benchmark 网络访问应显式标记并可关闭。 | 中 |
| 审计日志 | run、case、grader、Agent trace 必须可审计。 | 高 |

#### 6.3.2 隐私需求

| 隐私类型 | 需求描述 | 合规要求 |
|----------|----------|----------|
| 数据收集 | 仅收集 benchmark 执行所需轨迹和指标。 | 本地优先 |
| 数据存储 | benchmark 结果默认存储在本地 app data / SQLite。 | 用户可清理 |
| 数据共享 | 不自动上传 benchmark 结果。 | 用户显式导出 |

### 6.4 可测试性

| 测试类型 | 测试要求 | 覆盖率目标 |
|----------|----------|------------|
| 单元测试 | importer、metric aggregator、grader parser、comparison 纯函数必须覆盖。 | 核心逻辑 80%+ |
| 集成测试 | 使用小型 fixture suite 跑通 import-run-grade-aggregate。 | 关键路径 100% |
| 端到端测试 | UI/IPC 可查看 suite/run/result。 | 主要工作流覆盖 |

### 6.5 合规性

| 合规标准 | 适用范围 | 合规要求 |
|----------|----------|----------|
| GPL-3.0-only | 当前项目 license | 引入外部代码/数据需遵守其 license；默认不 vendoring GA 数据。 |
| Benchmark provenance | 外部 benchmark | 记录来源路径、来源仓库、commit 或文件 hash。 |

### 6.6 功能安全

| 安全等级 | 功能安全要求 | 验证方法 |
|----------|--------------|----------|
| 高 | 不可信 grader 不得任意污染系统路径。 | workspace 沙箱、超时、路径校验测试 |
| 高 | benchmark 不得自动执行破坏性用户仓库操作。 | 隔离 workspace 集成测试 |

### 6.7 用户文档需求

| 文档类型 | 内容要求 | 目标用户 |
|----------|----------|----------|
| 使用说明 | 如何导入 suite、运行 benchmark、理解指标 | 高级用户 |
| 指标说明 | TSR、token_per_success、tool_error_rate 等定义 | 研发/发布负责人 |
| 扩展说明 | 如何新增 benchmark adapter / grader | 开发者 |

---

## 7. 系统影响分析

### 7.1 影响列表

| 受影响模块 | 影响程度 | 说明 |
|------------|----------|------|
| `src-tauri/src/agent/engine.rs` | 中 | 需要确保 benchmark final response、事件、工具信息足够可采集；尽量不改变主执行语义。 |
| `src-tauri/src/db/migrations.rs` | 高 | 需要新增 benchmark suite/run/case/result/metric 表。 |
| `src-tauri/src/db/queries.rs` | 高 | 需要新增 benchmark 查询与聚合。 |
| `src-tauri/src/commands/` | 高 | 需要新增 benchmark IPC commands。 |
| `src/ipc/commands.ts` | 中 | 需要前端类型和命令 wrapper。 |
| `src/views/InsightsView.tsx` 或新 Benchmark View | 中 | 展示 run、指标和对比。 |
| `src-tauri/src/tools` | 低/中 | 若评测需要 final response 文件或工具 telemetry 增强，可能扩展。 |

### 7.2 影响分析

- 对现有 Mission 流程：应保持无行为变化，benchmark runner 作为新入口复用 AgentEngine。
- 对数据库：新增表和索引，不应重写现有表；需要关联 agent_id 以复用 trace。
- 对安全策略：benchmark 会批量触发 LLM、shell、grader，需要配置化预算与权限。
- 对 UI：可先接入 Insights/Settings，后续独立 Benchmark Dashboard。

### 7.3 风险评估

| 风险 | 影响 | 缓解措施 |
|------|------|----------|
| Grader 执行不安全 | 可能读取/写入非预期路径 | 受控 workspace、超时、参数白名单、后续进程沙箱 |
| 指标不可比 | 不同 run 配置差异导致误判 | 保存完整元数据并在对比时提示差异 |
| final response 捕获不完整 | 直接回答型 case 无法评分 | 标准化 final response 采集：task_complete summary + message event + 可选 final_response.txt |
| GA 数据集部分依赖 web/特殊能力 | Coding Agent 当前工具集不完全覆盖 | 标记 capability gap，允许 case skip/unsupported |
| 大规模 benchmark 成本高 | 误触发高 LLM 成本 | run 级预算、case 数量确认、停止按钮 |
| 事件 schema 不足 | 工具效率指标缺字段 | 新增 benchmark metric extractor 或增强 `agent_events.meta` |

---

## 8. GA-Technical-Report 研究摘要

### 8.1 仓库结构发现

GA-Technical-Report 仓库包含技术报告 PDF、README、图片和 datasets。仓库本身不包含 GA 主实现和历史运行轨迹。与本功能相关的数据集：

- `datasets/sop_bench`：20 条危险品订单 SOP 样本，评估多步 SOP 完成和 TSR。
- `datasets/lifelong_agentbench`：DB-Bench 20 条 SQL 任务，使用 SQL/MD5 等参考答案。
- `datasets/realfin_benchmark`：40 条金融分析任务，包含 prompt、expected_behavior、reference answer、automated_checks、hybrid judge 配置。
- `datasets/tool_efficiency_benchmark`：16 条工具效率任务，包括 11 条 simple tool-generalization 和 5 条 long-horizon complex，包含 assets 和 Python graders。

### 8.2 Task Completion & Token Efficiency 映射

GA 维度问题：能否以更低 token 成本完成困难任务。Miragenty 映射指标：

- `task_success` / `task_success_rate`。
- `input_tokens`、`output_tokens`、`total_tokens`。
- `cost_usd`。
- `tokens_per_success`。
- `requests_per_success`。
- 成功样本与失败样本 token 分布差异。

### 8.3 Tool-Use Efficiency 映射

GA 维度问题：能否用更少工具 schema/调用开销完成专用工具能做的任务。README 和雷达图强调：Task Success Rate、Avg Number of Tokens、Avg Number of Requests、Avg Number of Tool Calls。Miragenty 映射指标：

- `tool_call_count`。
- `tool_call_count_by_name`。
- `tool_error_count` / `tool_error_rate`。
- `llm_request_count`。
- `total_tokens`。
- `task_success`。
- `runtime_ms`。
- `read_only_loop_hint_count`、`guardrail_retry_count`、`recovery_attempt_count` 作为 Miragenty 特有诊断指标。

### 8.4 与当前 Miragenty 的可接入点

- `AgentEngine` 已持久化 `agent_events`，包括 `llm_call`、`tool_use`、`tool_result`、`message`、`status_change`、`error`、`guardrail_*`、`recovery_*`、`hook_*`。
- `AgentEngine` 已持久化 `cost_records` 并累计 `agents.tokens_used`、`agents.cost_usd`。
- `task_complete` 是明确完成信号，可作为 sample finalization 的关键事件。
- Scheduler 和 `run_agent` 已能以 workspace + task_description 启动 Coding Agent；benchmark runner 可以复用但需要同步等待和 sample 级状态管理。

---

## 9. 成功标准

- 能导入 GA Tool Efficiency Benchmark 并识别 16 个 case。
- 能对至少一个 fixture suite 完成端到端 import-run-grade-aggregate。
- 能从现有 Agent trace 计算 token/request/tool/success 指标。
- 能保存 run 和 sample result，并在 app 重启后查看。
- 能比较两个 run，并输出关键 delta。
- 不影响普通 Mission / Coding Agent 执行流程。
