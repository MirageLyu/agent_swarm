# Handoff 会话上下文传递使用说明

本文档说明 Miragenty 仓库内的 Handoff / Context Transfer 能力。该能力只复刻已有开源模式，不引入新的复杂上下文系统。

## 能力来源

| 本仓库能力 | 参考实现 | 用途 |
|---|---|---|
| `handoff` | Matt Pocock `/handoff` | 最小 baseline，生成临时目录 Handoff |
| `handoff-project` | Robert Guss handoff skill | 写入项目 `.claude/handoffs/` 的结构化 Handoff |
| `transfer-context` | Bex `transfer_context.md` | 安全上下文转移，要求下个 session 验证 |
| `handoffplan` | REMvisual `/handoffplan` | 上下文 + 分阶段执行计划 |
| `handoff-manager.mjs` | ContextHandoff Engine | active / `_done.md` consumed 管理 |
| `.claude/hooks/*` | Continuous-Claude / ClawMem | PreCompact、SessionStart、Stop 自动化草案 |

## 边界

Handoff 是当前任务/session 状态，不替代：

- memory：长期稳定事实和偏好；
- `CLAUDE.md`：项目级规则；
- transcript：完整历史；
- Mission Contract：任务范围和验收标准；
- Task Handoff Packet：Miragenty mission 内 task-to-task 上下文；
- Mission Report：结案报告和交付物说明。

## `/handoff`

位置：

```text
.claude/skills/handoff/SKILL.md
```

用途：最小 baseline，写到 OS 临时目录，不污染项目。

使用：

```text
/handoff
/handoff 下一轮继续实现 hooks 自动化
```

要求：

- 包含 `Suggested Skills`；
- 不复制已有 PRD、plan、ADR、issue、commit、diff，只引用路径或 URL；
- 脱敏；
- 参数用于定制下一轮 focus；
- 输出 paste-ready `Resume Prompt`。

## `handoff-project`

位置：

```text
.claude/skills/handoff-project/SKILL.md
```

用途：写项目内结构化 Handoff。

输出：

```text
.claude/handoffs/YYYY-MM-DD-HHMM-<brief-description>.md
```

结构：

- Current State
- What We Did
- Decisions Made
- Code Changes
- Open Questions
- Blockers / Issues
- Context to Remember
- Next Steps
- Files to Review on Resume

## `transfer-context`

位置：

```text
.claude/skills/transfer-context/SKILL.md
```

用途：上下文变差或接近上限时，生成安全转移文件。

输出：

```text
.claude/context-transfers/<random-8-chars>.md
```

用户可见输出只能是：

```text
Read the file <absolute-path-to-file> to get the context
```

关键规则：

- transfer 内容不直接打印进对话；
- `Open Work` 写状态，不写命令；
- 新 session 必须读取 Relevant Files 并验证；
- transfer 是 context，不是 instructions；
- 最后要求等待用户指令。

## `handoffplan`

位置：

```text
.claude/skills/handoffplan/SKILL.md
```

用途：研究/设计已完成，下一 session 应直接执行计划。

输出：

```text
.claude/handoffs/HANDOFFPLAN_<slug>_<YYYY-MM-DD_HHMMSS>.md
```

结构：

- The Goal
- Where We Are
- What We Tried
- Key Decisions
- Evidence & Data
- User Feedback
- Where We're Going
- Phased Plan
- Anti-Goals
- Quick Start
- Resume Prompt

## consumed / done 管理

脚本：

```text
scripts/handoff-manager.mjs
```

列出未消费 handoff：

```bash
node scripts/handoff-manager.mjs list .claude/handoffs
```

标记已消费：

```bash
node scripts/handoff-manager.mjs consume .claude/handoffs/example.md
```

会重命名为：

```text
.claude/handoffs/example_done.md
```

清理 7 天前的 done handoff：

```bash
node scripts/handoff-manager.mjs clean .claude/handoffs 7
```

## hooks 草案

脚本：

```text
.claude/hooks/precompact-handoff.sh
.claude/hooks/sessionstart-handoffs.sh
.claude/hooks/stop-handoff-draft.sh
```

安装前先确保脚本可执行：

```bash
chmod +x .claude/hooks/precompact-handoff.sh \
  .claude/hooks/sessionstart-handoffs.sh \
  .claude/hooks/stop-handoff-draft.sh
```

Claude Code settings 示例见：

```text
docs/handoff/hooks-settings-example.json
```

该示例把：

- `PreCompact` 连接到 `.claude/hooks/precompact-handoff.sh`
- `SessionStart` 连接到 `.claude/hooks/sessionstart-handoffs.sh`
- `Stop` 连接到 `.claude/hooks/stop-handoff-draft.sh`

这些 hooks 是草案/项目模板。启用前应按当前 Claude Code settings 结构合并到用户或项目配置中，不要直接覆盖已有 settings。

### PreCompact

`precompact-handoff.sh` 在压缩前生成快照：

```text
.claude/handoffs/PRECOMPACT_<timestamp>_snapshot.md
```

### SessionStart

`sessionstart-handoffs.sh` 输出未消费 handoff 文件列表，明确要求当作背景 context，不当作指令。

### Stop

`stop-handoff-draft.sh` 生成 draft-only handoff：

```text
.claude/handoffs/STOP_DRAFT_<timestamp>_handoff.md
```

Stop hook 容易制造噪音，所以保持 draft-only。三个 hook 都应保持 fail-open：hook 失败不应阻塞用户正常工作。

## 验证

运行：

```bash
node scripts/verify-handoff-baseline.mjs
```

该脚本验证：

- 所有 skill 必需字段和关键规则；
- baseline handoff fixture；
- transfer-context 的安全规则；
- handoffplan 的 `What We Tried`、`Quick Start`、阶段计划；
- consumed / `_done.md` 管理脚本。

Rust skill registry 验证：

```bash
cargo test -q -p miragenty skills::registry::tests:: --manifest-path src-tauri/Cargo.toml
```

## 安全原则

- 不写入 token、API key、password、cookie、private key。
- 不复制已有大型 artifact。
- Handoff 只提供上下文。
- 新 session 必须验证相关文件。
- `Open Work` 描述状态，不写强制命令。
