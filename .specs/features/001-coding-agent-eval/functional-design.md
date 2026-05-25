# 功能设计说明书: Coding Agent 测评系统

**功能分支**: `001-coding-agent-eval`  
**创建日期**: 2026-05-22  
**状态**: 已实现首版  
**关联需求文档**: [需求分析说明书](requirements-analysis.md)

---

## 1. 功能概述

为 Miragenty Coding Agent 新增开发团队使用的本地 benchmark/evaluation harness，支持导入 GA-Technical-Report 风格数据集，创建并执行 Coding Agent benchmark run，采集 task completion、token efficiency 和 tool-use efficiency 指标，并提供后端/测试层 summary、compare、export 能力。该能力不进入产品前端导航，不影响终端用户产品界面。

首版支持：

- GA Tool Efficiency Benchmark JSONL 导入。
- GA SOP-Bench CSV + SOP 导入。
- suite/case/run/result/metric/grader artifact 持久化。
- 顺序执行 Coding Agent case，按 case 创建隔离 workspace。
- Python grader 执行与 stdout JSON 解析。
- 从 `agent_events` 与 `cost_records` 聚合 token、request、tool、error、runtime 与 TSR 指标。
- JSON / Markdown / CSV 导出。
- 两个 run 的共同 case 对比。
- 前端 Benchmark View 基础工作流。

---

## 2. 后端设计

### 2.1 模块划分

新增 `src-tauri/src/benchmark/`：

- `types.rs`：benchmark suite/case/run/result/metrics/summary 类型。
- `importer.rs`：GA Tool Efficiency 与 SOP-Bench 导入器。
- `runner.rs`：workspace 准备、AgentEngine 执行、grader 与 metrics 串联。
- `grader.rs`：受 timeout 限制的 Python grader 执行。
- `metrics.rs`：从 agent trace 和 cost records 提取 case/run 指标。
- `compare.rs`：baseline/candidate run 对齐与 delta 分类。
- `export.rs`：summary 的 JSON/Markdown/CSV 输出。

### 2.2 数据库

新增 migration `030_benchmark_evaluation`，包含：

- `benchmark_suites`
- `benchmark_cases`
- `benchmark_runs`
- `benchmark_results`
- `benchmark_metric_snapshots`
- `benchmark_grader_artifacts`

查询层在 `src-tauri/src/db/queries.rs` 提供 row struct 和 CRUD/list helper，保持现有 SQLite helper 风格。

### 2.3 Runner 流程

1. 根据 run 的 case ids 读取 case。
2. 每个 case 在 benchmark workspace root 下创建独立目录。
3. 复制 case assets，避免修改 benchmark 源目录或 Miragenty 项目目录。
4. 创建 benchmark agent 记录。
5. 复用 `AgentEngine::run_with_options` 执行 Coding Agent。
6. 保存 final response 到 workspace。
7. 如有 grader，执行 `python3 grader --workspace ... --response-file ...`。
8. 写入 result、grader artifact、case metric snapshot。
9. run 完成后写入 run metric snapshot。

### 2.4 IPC Commands

新增 `src-tauri/src/commands/benchmark.rs` 并注册：

- `import_benchmark_suite`
- `list_benchmark_suites`
- `get_benchmark_suite`
- `create_benchmark_run`
- `start_benchmark_run`
- `cancel_benchmark_run`
- `get_benchmark_run`
- `list_benchmark_results`
- `get_benchmark_summary`
- `compare_benchmark_runs`
- `export_benchmark_run`

---

## 3. 开发侧使用方式

首版测评系统定位为开发团队内部能力，不接入产品前端。核心能力通过 Rust benchmark 模块、数据库持久化和测试/后续 CLI 驱动使用：

- 导入本地 GA benchmark suite。
- 创建并执行 Coding Agent run。
- 查询 run/result/metric/grader artifact。
- 输出 JSON/Markdown/CSV summary。
- 对比两个 run 的共同 case delta。

如未来需要可再单独增加 dev-only CLI 或隐藏开发工具入口，但不应出现在普通产品导航中。

---

## 4. DFX 分析

### 安全性

- Benchmark assets 复制到隔离 workspace 后执行，避免污染源数据集和项目目录。
- Python grader 使用固定 `python3`、显式参数、空 stdin、stdout/stderr 捕获和 timeout。
- 首版不自动下载外部 benchmark，不 vendoring 外部资产。

### 可复现性

- suite/case/raw JSON、run config、case ids、metrics 和 grader artifacts 均持久化。
- run workspace path 记录在 result 中，便于复查。

### 可扩展性

- `BenchmarkSourceKind` 预留 Lifelong AgentBench、RealFin 和 custom。
- metrics snapshot 用 scope 区分 case/run，可扩展更多聚合维度。
- runner 首版顺序执行，schema 保留后续并发和预算控制字段。

---

## 5. 验证

已执行：

- `cargo test --manifest-path src-tauri/Cargo.toml`：通过。
- `pnpm build`：通过。
- `pnpm test`：通过。

待完成：
