# 源码修改清单: Coding Agent 测评系统

**功能分支**: `001-coding-agent-eval`  
**更新日期**: 2026-05-22  
**状态**: 开发侧首版实现完成

---

## 新增文件

### 后端 benchmark 模块

- `src-tauri/src/benchmark/mod.rs` — benchmark 模块导出入口。
- `src-tauri/src/benchmark/types.rs` — suite/case/run/result/metrics/summary 类型。
- `src-tauri/src/benchmark/importer.rs` — GA Tool Efficiency 与 SOP-Bench 导入器。
- `src-tauri/src/benchmark/metrics.rs` — case/run 指标提取与聚合。
- `src-tauri/src/benchmark/grader.rs` — Python grader 执行与 artifact 转换；自动检测 grader 是否声明 `--response-file`/`--response_file`，兼容仅接受 `--workspace` 的 GA 长任务 grader。
- `src-tauri/src/benchmark/compare.rs` — run baseline/candidate 对比。
- `src-tauri/src/benchmark/export.rs` — JSON/Markdown/CSV 导出。
- `src-tauri/src/benchmark/runner.rs` — Coding Agent benchmark case runner 与 run orchestration；新增 per-case trace 导出，包含 prompt、final response、agent events、grader artifacts、case metrics；修复 benchmark result/agent 外键写入顺序，支持 `--case-id` 使用 GA task_id 或 DB id；新增 CLI 运行中 per-case 进度输出和执行失败落库；benchmark final response 以 `task_complete.summary` 作为提交给 grader 的最终答案，并为直接输出类 case 注入格式 guardrail，避免把“我已经输出了 JSON”这类 meta-summary 误判为最终答案；GA case assets 现在保留原始 `assets/<task_id>/...` 路径的同时，也 flatten 到 workspace root，匹配 prompt/grader 对 `analysis.ipynb`、`script.txt` 等根目录文件的预期。

### 开发侧 CLI

- `src-tauri/src/bin/benchmark_runner.rs` — dev-only benchmark runner CLI；通过独立 YAML/JSON benchmark pipeline config 注入 provider/model/API key/data/workspace/report 配置，不接入产品前端，不读取产品 app data/config；每次 run 在 reports/traces 下导出 per-case trace。

### SDD

- `.specs/features/001-coding-agent-eval/functional-design.md` — 功能设计说明书。
- `.specs/features/001-coding-agent-eval/source-code-changes.md` — 本源码修改清单。

---

## 修改文件

### 后端

- `src-tauri/src/agent/engine.rs`
  - 修复 tool dispatch 日志和 argument parse 错误提示中的 UTF-8 非 char boundary 截断 panic。
  - 新增 `assistant_text` event，用于保留 LLM 最后一次自然文本输出，供 benchmark trace 使用。
  - 支持无产品 task 记录的 dev-only benchmark agent 运行显式 completion guardrail，避免 benchmark runner 的格式校验被直接放行。
  - `publish_artifact` 在无产品 task binding 的 dev-only benchmark agent 下改为 no-op success，避免污染 benchmark tool error 指标；产品 task 绑定存在时仍走原 artifact 持久化路径。

- `src-tauri/src/tools/executor.rs`
  - `shell_exec` 新增 `timeout_seconds` / `idle_timeout_seconds` 显式命令级 timeout 参数，避免单个网络/下载命令吃满整个 Agent timeout；子进程继续继承当前环境变量，包括 `ALL_PROXY`/`HTTPS_PROXY` 等代理配置。

- `src-tauri/src/tools/registry.rs`
  - 更新 `shell_exec` tool schema 与说明，向 Agent 暴露显式 timeout 参数和代理环境继承语义。

- `src-tauri/src/tools/definitions.rs`
  - 同步 legacy `shell_exec` tool definition 的 timeout schema 与说明。

- `src-tauri/src/agent/guardrail.rs`
  - 新增 `summary_matches` guardrail，支持校验 `task_complete.summary` 是否为 JSON code block、text code block 或精确 `OK`，用于 benchmark 直接输出类任务的最终答案格式约束。

- `src-tauri/src/db/migrations.rs`
  - 新增 migration `030_benchmark_evaluation`。
  - 创建 benchmark suites/cases/runs/results/metrics/grader artifacts 表。

- `src-tauri/src/db/queries.rs`
  - 新增 benchmark row struct。
  - 新增 suite/case/run/result/metrics/grader artifact 查询与写入 helper。
  - 新增 per-case trace 所需的 result metric 与 grader artifact 查询 helper。

- `src-tauri/src/commands/config.rs`
  - 新增 `ConfigManager::from_config`，供 dev-only benchmark CLI 注入独立 pipeline config，避免读取产品配置文件。

- `src-tauri/src/lib.rs`
  - 导出 `benchmark` Rust 模块，供开发侧 harness/测试调用。
  - 未注册产品 Tauri IPC command，避免进入用户产品界面。

- `src-tauri/src/agent/mod.rs`
  - re-export `AgentRunOptions`，供 benchmark runner 复用 AgentEngine options。

- `src-tauri/src/llm/openai_compat.rs`
  - OpenAI-compatible message conversion 跳过只有 reasoning、没有 content/tool_calls 的 assistant message，避免 DeepSeek/OpenAI-compatible API 返回 `Invalid assistant message: content or tool_calls must be set`。

---

## 明确未修改范围

- 未新增产品前端页面。
- 未新增 Sidebar/Command Palette 导航入口。
- 未新增前端 IPC wrapper。
- 未新增产品 i18n 文案。

---

## 最新 benchmark 归档

- 2026-05-25，GA Tool Efficiency 全量 run：`59dadb81-f502-47ea-b9cd-17d7c1881305`。
  - 配置：`openai_compat` / `deepseek-v4-pro`，dev-only benchmark pipeline，独立 workspace/report 目录。
  - 结果：16 cases，12 success，`all_cases_tsr=0.75`，`graded_cases_tsr=0.80`。
  - 总消耗：`1,607,945` tokens，`$3.43583`，148 LLM requests，183 tool calls，6 tool errors。
  - 成功覆盖：glob/grep/webfetch fallback/websearch fallback/CSV/XLSX/browser static extract/subagent fallback/SQL/long research 等 12 个 case。
  - 待修问题：`teb_06` 与 `teb_10` 暴露 workspace asset layout 与 grader 预期不一致；`publish_artifact` 在 dev-only benchmark agent 下因无产品 task binding 报错；`teb_12` 受本地网络与 shell 长命令 timeout 影响，需要命令级 timeout 与代理环境继承确认；`teb_14` 仅因报告长度约束失败，后续应补产物自检 guardrail。

---

## 验证记录

- `cargo test --manifest-path src-tauri/Cargo.toml prepare_case_workspace_flattens_ga_case_assets_to_root`：通过，覆盖 GA asset flatten 到 workspace root，修复 `teb_06`/`teb_10` grader 找根目录文件的问题。
- `cargo test --manifest-path src-tauri/Cargo.toml shell_explicit_timeout_seconds_is_enforced`：通过，覆盖 `shell_exec` 显式 `timeout_seconds` 会触发 watchdog kill。
- `cargo test --manifest-path src-tauri/Cargo.toml benchmark`：通过，8 个 benchmark 相关测试通过。
- `cargo check --manifest-path src-tauri/Cargo.toml --bin benchmark_runner`：通过。
- `cargo run --manifest-path src-tauri/Cargo.toml --bin benchmark_runner -- --config /Volumes/T7/open_source/GA-Technical-Report/benchmark-pipeline.yml --suite /Volumes/T7/open_source/GA-Technical-Report/datasets/tool_efficiency_benchmark --case-id teb_10_tts_script_audio --name "GA Tool Efficiency Asset Flatten Smoke teb_10 - deepseek-v4-pro"`：通过，run `53e367d7-14e1-4df0-a609-01335a747f85`，`success=true`，确认 root `script.txt` 预期修复。
- `cargo run --manifest-path src-tauri/Cargo.toml --bin benchmark_runner -- --config /Volumes/T7/open_source/GA-Technical-Report/benchmark-pipeline.yml --suite /Volumes/T7/open_source/GA-Technical-Report/datasets/tool_efficiency_benchmark --case-id teb_06_notebook_inspect --name "GA Tool Efficiency Asset Flatten Smoke teb_06 - deepseek-v4-pro"`：通过运行与评分，run `572c6036-1378-480d-ba74-5e35654262f6`；grader 不再因缺失根目录 `analysis.ipynb` 崩溃，当前失败项变为 `release_snapshot_source_ok=false`（模型使用 `statistics.mean(values)`，grader 只认可源码/输出中出现具体 `7.25` 等值），属于 Notebook 任务完成策略问题而非 workspace layout 问题。

- `cargo check --manifest-path src-tauri/Cargo.toml --bin benchmark_runner`：通过，包含 per-case trace 导出编译验证。
- `cargo test --manifest-path src-tauri/Cargo.toml benchmark`：通过，7 个 benchmark 相关测试通过，覆盖 grader `--response-file` 自动适配。
- `cargo test --manifest-path src-tauri/Cargo.toml summary_matches`：通过，覆盖 benchmark final-answer guardrail 对 meta-summary 的拒绝与 JSON code block 放行。
- `cargo test --manifest-path src-tauri/Cargo.toml convert_messages_skips_reasoning_only_assistant_message`：通过，覆盖 OpenAI-compatible 空 assistant message 400 回归。
- `cargo check --manifest-path src-tauri/Cargo.toml --bin benchmark_runner`：通过，确认 dev-only benchmark runner 编译。
- `cargo run --manifest-path src-tauri/Cargo.toml --bin benchmark_runner -- --config /Volumes/T7/open_source/GA-Technical-Report/benchmark-pipeline.yml --suite /Volumes/T7/open_source/GA-Technical-Report/datasets/tool_efficiency_benchmark --case-id teb_01_glob_markdown_list --name "GA Tool Efficiency Guardrail Smoke 2 - deepseek-v4-pro"`：通过，`teb_01` 从 final-answer meta-summary 失败修复为 `success=true`。
- `cargo run --manifest-path src-tauri/Cargo.toml --bin benchmark_runner -- --config /Volumes/T7/open_source/GA-Technical-Report/benchmark-pipeline.yml --suite /Volumes/T7/open_source/GA-Technical-Report/datasets/tool_efficiency_benchmark --case-id teb_13_sql_copilot_query_generation --name "GA Tool Efficiency Grader Adapter Smoke - deepseek-v4-pro"`：通过，确认 workspace-only grader 不再收到不支持的 `--response-file` 参数，case 进入真实任务评分（`query_executable=false`）。

- `cargo test --manifest-path src-tauri/Cargo.toml char_safe_excerpt`：通过，覆盖中文 UTF-8 截断不 panic。
- `cargo run --manifest-path src-tauri/Cargo.toml --bin benchmark_runner -- --config /Volumes/T7/open_source/GA-Technical-Report/benchmark-pipeline.yml --suite /Volumes/T7/open_source/GA-Technical-Report/datasets/tool_efficiency_benchmark --case-id teb_01_glob_markdown_list --name "GA Tool Efficiency Smoke - deepseek-v4-pro"`：通过，生成 1 个 case result 与 per-case trace。
- `cargo run --manifest-path src-tauri/Cargo.toml --bin benchmark_runner -- --config /Volumes/T7/open_source/GA-Technical-Report/benchmark-pipeline.yml --suite /Volumes/T7/open_source/GA-Technical-Report/datasets/tool_efficiency_benchmark --case-id teb_01_glob_markdown_list --name "GA Tool Efficiency Progress Smoke - deepseek-v4-pro"`：通过，确认 CLI 输出 run start、case start、case finish 进度行与 trace 路径。
- `cargo run --manifest-path src-tauri/Cargo.toml --bin benchmark_runner -- --help`：通过，确认 CLI 使用独立 `--config <path>` benchmark pipeline 配置。
- `pnpm build`：通过。
