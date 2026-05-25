# 测试用例清单: Coding Agent 测评系统

**功能分支**: `001-coding-agent-eval`  
**创建日期**: 2026-05-22  
**状态**: 自动化验证通过（后端 Rust 测试、前端 build、前端 Vitest）；Tauri UI 手动导入/真实 LLM run 待验证  
**关联需求文档**: [需求分析说明书链接](requirements-analysis.md)

---

## 验证说明

本清单用于验证 Coding Agent 测评系统是否符合需求规格。所有正向场景必须通过；可选、边界和异常场景根据实现阶段选择性验证。

---

## 需求 FR-001: Benchmark Suite 导入

**需求描述**: 系统可从本地目录导入 GA 风格 benchmark 数据集并归一化为 suite/case。

### 正向场景（必选）

- [ ] **TC-FR001-001**: 导入 GA Tool Efficiency Benchmark
  - **前置条件**: 本地存在 GA-Technical-Report 仓库；包含 `datasets/tool_efficiency_benchmark/tool_efficiency_tasks.jsonl`。
  - **测试步骤**:
    1. 选择 GA Tool Efficiency Benchmark 目录。
    2. 执行导入。
    3. 查询导入后的 suite 和 case 列表。
  - **预期结果**: 系统识别 16 个 case，保留 task_id、task_type、source_suite、target_tool_or_capability、prompt、assets、expected_outputs、grader 和 raw JSON。
  - **验证状态**: ⏳ 待验证
  - **备注**: 对齐 GA README 中 11 simple + 5 long-horizon。

- [ ] **TC-FR001-002**: 导入 SOP-Bench 数据集
  - **前置条件**: 本地存在 `datasets/sop_bench/sop.txt` 和 `test_set_with_outputs.csv`。
  - **测试步骤**:
    1. 选择 SOP-Bench 目录。
    2. 执行导入。
    3. 查询 case 列表。
  - **预期结果**: 系统识别 20 个 case，并保存 expected_output 标签。
  - **验证状态**: ⏳ 待验证
  - **备注**: 用于 Task Completion & Token Efficiency。

### 异常场景（可选）

- [ ] **TC-FR001-ERR-001**: 缺失 JSONL 或 CSV 文件
  - **前置条件**: 数据集目录不完整。
  - **测试步骤**:
    1. 选择缺失核心文件的目录。
    2. 执行导入。
  - **预期结果**: 导入失败并返回结构化错误；不会创建半成品 suite，或半成品 suite 明确标记为 invalid。
  - **验证状态**: ⏳ 待验证
  - **备注**: 无。

---

## 需求 FR-002: Coding Agent Benchmark Runner

**需求描述**: 系统可批量运行 Coding Agent 处理 benchmark case。

### 正向场景（必选）

- [ ] **TC-FR002-001**: 单 case 运行闭环
  - **前置条件**: 已导入包含 assets 和 grader 的 fixture case；已配置 LLM provider 或 mock provider。
  - **测试步骤**:
    1. 创建 benchmark run，仅选择一个 case。
    2. 启动 run。
    3. 等待 case 完成。
    4. 查询 sample result。
  - **预期结果**: sample result 包含 workspace_path、agent_id、status、started_at、completed_at、final_response 或产物引用。
  - **验证状态**: ⏳ 待验证
  - **备注**: 初期可用 mock agent/fixture 降低 LLM 成本。

- [ ] **TC-FR002-002**: 多 case 顺序运行
  - **前置条件**: 已导入至少 3 个 case。
  - **测试步骤**:
    1. 创建包含 3 个 case 的 run。
    2. 启动 run。
    3. 观察每个 case 状态。
  - **预期结果**: 每个 case 使用独立 workspace，状态互不污染；run 最终进入 completed 或 completed_with_failures。
  - **验证状态**: ⏳ 待验证
  - **备注**: 后续并发扩展另测。

### 异常场景（可选）

- [ ] **TC-FR002-ERR-001**: Agent 超时
  - **前置条件**: case timeout 设置极小或 mock agent 故意不结束。
  - **测试步骤**:
    1. 启动 run。
    2. 等待超时。
  - **预期结果**: case 标记 timeout/failed，run 继续处理后续 case。
  - **验证状态**: ⏳ 待验证
  - **备注**: 无。

---

## 需求 FR-003: Task Completion 评分

**需求描述**: 系统可通过 grader/judge 判定 case 是否成功，并计算 suite TSR。

### 正向场景（必选）

- [ ] **TC-FR003-001**: 自动 grader 映射 task_success
  - **前置条件**: case grader 输出 JSON，包含 `task_success: true`。
  - **测试步骤**:
    1. 运行 case。
    2. 执行 grader。
    3. 查询 sample result。
  - **预期结果**: sample result 的 success=true，grader_raw_json 完整保存。
  - **验证状态**: ⏳ 待验证
  - **备注**: 使用 GA TEB grader fixture。

- [ ] **TC-FR003-002**: 聚合 TSR
  - **前置条件**: 一个 run 中存在成功、失败和 ungraded case。
  - **测试步骤**:
    1. 运行聚合计算。
    2. 查询 run summary。
  - **预期结果**: 同时给出 all_cases_tsr 和 graded_cases_tsr，分母定义清晰。
  - **验证状态**: ⏳ 待验证
  - **备注**: 无。

### 边界场景（可选）

- [ ] **TC-FR003-EDGE-001**: Grader 无 `task_success` 字段
  - **前置条件**: grader 输出多个细分分数但无总成功字段。
  - **测试步骤**:
    1. 执行 grader。
    2. 查询 success 状态。
  - **预期结果**: 按配置阈值推导成功，或标记为 ungraded；行为可解释。
  - **验证状态**: ⏳ 待验证
  - **备注**: 无。

---

## 需求 FR-004: Token Efficiency 指标

**需求描述**: 系统可记录并聚合 input/output/total token、成本和单位成功 token。

### 正向场景（必选）

- [ ] **TC-FR004-001**: 单 case token 指标采集
  - **前置条件**: Agent 执行产生 `cost_records`。
  - **测试步骤**:
    1. 运行一个 case。
    2. 查询 sample metrics。
  - **预期结果**: sample metrics 包含 input_tokens、output_tokens、total_tokens、cost_usd。
  - **验证状态**: ⏳ 待验证
  - **备注**: 来自现有 `cost_records`。

- [ ] **TC-FR004-002**: run 级 token_per_success 聚合
  - **前置条件**: run 包含至少一个成功 case。
  - **测试步骤**:
    1. 运行聚合。
    2. 检查 token_per_success。
  - **预期结果**: token_per_success = run total_tokens / successful_case_count。
  - **验证状态**: ⏳ 待验证
  - **备注**: 无。

### 边界场景（可选）

- [ ] **TC-FR004-EDGE-001**: 0 个成功 case
  - **前置条件**: run 中所有 case 均失败。
  - **测试步骤**:
    1. 聚合指标。
  - **预期结果**: token_per_success 为 null/undefined，不发生除零错误。
  - **验证状态**: ⏳ 待验证
  - **备注**: 无。

---

## 需求 FR-005: Tool-Use Efficiency 指标

**需求描述**: 系统可记录并聚合工具调用数、工具错误数、工具类别、请求数、runtime 等。

### 正向场景（必选）

- [ ] **TC-FR005-001**: 从 agent_events 提取工具指标
  - **前置条件**: Agent 产生 `llm_call`、`tool_use`、`tool_result` 事件。
  - **测试步骤**:
    1. 运行 case。
    2. 运行 metric extractor。
    3. 查询 sample metrics。
  - **预期结果**: metrics 包含 llm_request_count、tool_call_count、tool_result_count、tool_error_count、tool_call_count_by_name。
  - **验证状态**: ⏳ 待验证
  - **备注**: `tool_result.meta.is_error` 或内容中的 error 需可识别。

- [ ] **TC-FR005-002**: 工具效率聚合
  - **前置条件**: run 中多个 case 有不同工具调用轨迹。
  - **测试步骤**:
    1. 聚合 run metrics。
    2. 查看按 target capability 的指标。
  - **预期结果**: 展示平均工具调用数、平均请求数、工具错误率、tool_calls_per_success、requests_per_success。
  - **验证状态**: ⏳ 待验证
  - **备注**: 对齐 GA 雷达图维度。

### 可选场景（可选）

- [ ] **TC-FR005-OPT-001**: Miragenty 特有诊断指标
  - **前置条件**: trace 中存在 guardrail retry、recovery_attempt 或 read-only loop hint。
  - **测试步骤**:
    1. 运行 metric extractor。
  - **预期结果**: metrics 包含 guardrail_retry_count、recovery_attempt_count、read_only_loop_hint_count。
  - **验证状态**: ⏳ 待验证
  - **备注**: 用于定位单 Agent 能力短板。

---

## 需求 FR-006: Grader 执行与结果存储

**需求描述**: 系统可执行自动 grader，解析输出并保存细分评分。

### 正向场景（必选）

- [ ] **TC-FR006-001**: 执行 Python grader
  - **前置条件**: case 有 grader 脚本和 final_response.txt。
  - **测试步骤**:
    1. 运行 grader executor。
    2. 查询 grader result。
  - **预期结果**: 保存 exit_code、stdout_json、stderr、duration_ms；stdout JSON 可解析。
  - **验证状态**: ⏳ 待验证
  - **备注**: 传入 `--workspace` 和 `--response-file`。

### 异常场景（可选）

- [ ] **TC-FR006-ERR-001**: Grader 非零退出
  - **前置条件**: grader 脚本故意抛错。
  - **测试步骤**:
    1. 执行 grader。
  - **预期结果**: sample result 保留 Agent 结果，grading_status=failed，stderr 可查看。
  - **验证状态**: ⏳ 待验证
  - **备注**: 无。

---

## 需求 FR-007: Run 对比

**需求描述**: 用户可选择两个同 suite run 进行横向对比。

### 正向场景（必选）

- [ ] **TC-FR007-001**: 对比两个同 suite run
  - **前置条件**: 存在两个同 suite、case 集合相同的 run。
  - **测试步骤**:
    1. 选择 baseline 和 candidate。
    2. 执行 compare。
  - **预期结果**: 返回成功率、token、工具调用、请求数、runtime 的 aggregate delta 和 per-case delta。
  - **验证状态**: ⏳ 待验证
  - **备注**: 无。

### 边界场景（可选）

- [ ] **TC-FR007-EDGE-001**: case 集合不完全一致
  - **前置条件**: 两个 run 只部分 case 重叠。
  - **测试步骤**:
    1. 执行 compare。
  - **预期结果**: 只比较共同 case，并提示覆盖率和缺失 case。
  - **验证状态**: ⏳ 待验证
  - **备注**: 无。

---

## 需求 FR-008: 可复现实验元数据

**需求描述**: 每次 run 必须记录完整复现条件。

### 正向场景（必选）

- [ ] **TC-FR008-001**: Run 元数据保存
  - **前置条件**: 创建 benchmark run。
  - **测试步骤**:
    1. 创建 run。
    2. 查询 run metadata。
  - **预期结果**: metadata 包含 git commit、dirty flag、provider、model、Agent 配置、benchmark source path、case hash、timeout/budget 设置。
  - **验证状态**: ⏳ 待验证
  - **备注**: base_url 可保存 hash 或脱敏值。

---

## 需求 FR-009: UI/API 查询

**需求描述**: 前端或后续 CLI 可查询 suite、run、sample result 和 metric summary。

### 正向场景（必选）

- [ ] **TC-FR009-001**: 查询 suite/run/result
  - **前置条件**: 已导入 suite 并完成至少一个 run。
  - **测试步骤**:
    1. 调用 list suites。
    2. 调用 list runs。
    3. 调用 get run summary。
    4. 调用 get sample result。
  - **预期结果**: API 返回结构化数据，前端类型可表达全部字段。
  - **验证状态**: ⏳ 待验证
  - **备注**: 无。

---

## 需求 FR-010: Agent 类型扩展

**需求描述**: 评测系统核心数据模型和 runner 抽象应支持未来扩展到其他 Agent。

### 正向场景（必选）

- [ ] **TC-FR010-001**: 记录 agent_kind
  - **前置条件**: 创建 Coding Agent benchmark run。
  - **测试步骤**:
    1. 查询 run 和 sample result。
  - **预期结果**: run 或 sample result 明确记录 `agent_kind = coding`；schema 允许未来 planner/evaluator/chat。
  - **验证状态**: ⏳ 待验证
  - **备注**: 无。

---

## 需求 FR-011: 安全与预算控制

**需求描述**: benchmark 执行必须支持预算、超时、网络/命令权限和 workspace 隔离策略。

### 正向场景（必选）

- [ ] **TC-FR011-001**: Run 级预算限制
  - **前置条件**: 设置极低 token/cost 预算。
  - **测试步骤**:
    1. 启动 run。
    2. 触发预算阈值。
  - **预期结果**: run 停止或等待审批，未执行 case 不启动，已完成结果保留。
  - **验证状态**: ⏳ 待验证
  - **备注**: 具体策略在设计阶段细化。

- [ ] **TC-FR011-002**: Workspace 隔离
  - **前置条件**: case assets 包含多文件目录。
  - **测试步骤**:
    1. 启动 case。
    2. 检查源 benchmark 目录和用户项目目录。
  - **预期结果**: Agent 和 grader 只修改隔离 workspace；源目录不变。
  - **验证状态**: ⏳ 待验证
  - **备注**: 无。

---

## 需求 FR-012: 导出报告

**需求描述**: 用户可导出 benchmark run 报告供归档和讨论。

### 正向场景（必选）

- [ ] **TC-FR012-001**: 导出 Markdown/JSON/CSV
  - **前置条件**: 已完成一个 run。
  - **测试步骤**:
    1. 请求 JSON 导出。
    2. 请求 Markdown 导出。
    3. 请求 CSV 导出。
  - **预期结果**: JSON 包含完整结构化数据；Markdown 包含摘要、配置、指标表和失败 case；CSV 包含 per-case 指标。
  - **验证状态**: ⏳ 待验证
  - **备注**: 无。

---

## 验证统计

### 总体统计

| 统计项 | 数量 | 已验证 | 通过 | 失败 | 跳过 |
|--------|------|--------|------|------|------|
| 正向场景（必选） | 18 | 0 | 0 | 0 | 0 |
| 可选场景 | 1 | 0 | 0 | 0 | 0 |
| 逆向场景 | 0 | 0 | 0 | 0 | 0 |
| 边界场景 | 4 | 0 | 0 | 0 | 0 |
| 异常场景 | 4 | 0 | 0 | 0 | 0 |
| **总计** | **27** | **0** | **0** | **0** | **0** |

### 需求覆盖统计

| 需求ID | 正向场景 | 可选场景 | 逆向场景 | 边界场景 | 异常场景 | 状态 |
|--------|----------|----------|----------|----------|----------|------|
| FR-001 | 2/2 | 0/0 | 0/0 | 0/0 | 1/1 | ⏳ 待验证 |
| FR-002 | 2/2 | 0/0 | 0/0 | 0/0 | 1/1 | ⏳ 待验证 |
| FR-003 | 2/2 | 0/0 | 0/0 | 1/1 | 0/0 | ⏳ 待验证 |
| FR-004 | 2/2 | 0/0 | 0/0 | 1/1 | 0/0 | ⏳ 待验证 |
| FR-005 | 2/2 | 1/1 | 0/0 | 0/0 | 0/0 | ⏳ 待验证 |
| FR-006 | 1/1 | 0/0 | 0/0 | 0/0 | 1/1 | ⏳ 待验证 |
| FR-007 | 1/1 | 0/0 | 0/0 | 1/1 | 0/0 | ⏳ 待验证 |
| FR-008 | 1/1 | 0/0 | 0/0 | 0/0 | 0/0 | ⏳ 待验证 |
| FR-009 | 1/1 | 0/0 | 0/0 | 0/0 | 0/0 | ⏳ 待验证 |
| FR-010 | 1/1 | 0/0 | 0/0 | 0/0 | 0/0 | ⏳ 待验证 |
| FR-011 | 2/2 | 0/0 | 0/0 | 0/0 | 0/0 | ⏳ 待验证 |
| FR-012 | 1/1 | 0/0 | 0/0 | 0/0 | 0/0 | ⏳ 待验证 |

---

## 验证记录

### 验证会话记录

| 日期 | 验证人 | 验证场景数 | 通过 | 失败 | 备注 |
|------|--------|------------|------|------|------|
| 2026-05-22 | Claude | 0 | 0 | 0 | 初始创建 |
| 2026-05-25 | Claude | 16 | 12 | 4 | GA Tool Efficiency 全量 run `59dadb81-f502-47ea-b9cd-17d7c1881305`；12/16 成功，`teb_06`/`teb_10` 为 asset layout 问题，`teb_12` 为网络下载/长命令 timeout，`teb_14` 为报告长度约束失败 |
| 2026-05-25 | Claude | 5 | 5 | 0 | 修复后验证：benchmark 测试 8/8 通过；asset flatten 单测通过；shell explicit timeout 单测通过；benchmark_runner 编译通过；`teb_10` smoke 成功，`teb_06` grader 不再崩溃但 case 仍因 notebook source 具体值约束失败 |

### 失败记录

| 场景ID | 失败日期 | 失败原因 | 修复日期 | 修复说明 |
|--------|----------|----------|----------|----------|
| - | - | - | - | - |

---

## 验证完成确认

### 正向场景验证确认

- [ ] 所有正向场景已验证通过
- [ ] 正向场景验证失败已全部修复

### 可选场景验证确认（如适用）

- [ ] 可选场景已选择性验证
- [ ] 逆向场景已选择性验证
- [ ] 边界场景已选择性验证
- [ ] 异常场景已选择性验证

### 最终确认

- [ ] **我确认已完成所有必选场景的验证，功能实现符合需求规格**
- [ ] **我选择自行验证，不使用此技能进行验证**

**确认人**: ________________  
**确认日期**: ________________  
**签名**: ________________

---

## 变更历史

| 版本 | 日期 | 作者 | 变更说明 |
|------|------|------|----------|
| 1.0 | 2026-05-22 | Claude | 初始版本 |
