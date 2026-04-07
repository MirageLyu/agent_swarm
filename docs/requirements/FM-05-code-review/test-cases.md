# FM-05: Code Review & Diff — 测试用例

> 版本: v1.0 | 日期: 2026-04-01

---

## 单元测试 (UT)

### UT-01: Diff 解析（Rust）


| ID      | 场景    | 输入       | 期望结果      |
| ------- | ----- | -------- | --------- |
| UT-01.1 | 单文件修改 | patch 文本 | 解析出 1 个文件 |
| UT-01.2 | 多文件修改 | patch 文本 | 文件树正确     |
| UT-01.3 | 无变更   | 空 patch  | 返回空列表     |


### UT-02: Review 状态管理（TS）


| ID      | 场景                  | 操作                  | 期望结果             |
| ------- | ------------------- | ------------------- | ---------------- |
| UT-02.1 | 切换 Agent            | change active agent | 展示对应 diff        |
| UT-02.2 | 提交 approve          | submit action       | 本地状态更新为 approved |
| UT-02.3 | revision 必填 comment | 空 comment 提交        | 前端校验失败           |


---

## 集成测试 (IT)

### IT-01: 查看 Agent Diff


| 步骤  | 操作               | 期望结果             |
| --- | ---------------- | ---------------- |
| 1   | 运行一个会修改文件的 Agent | worktree 中有 diff |
| 2   | 打开 Review 视图     | 看到 Agent 标签      |
| 3   | 选择 Agent         | 看到变更文件树和 diff    |


### IT-02: Request Revision 记录


| 步骤  | 操作                                 | 期望结果                            |
| --- | ---------------------------------- | ------------------------------- |
| 1   | 在 ReviewView 点击 `Request Revision` | 弹出反馈输入框                         |
| 2   | 输入修订意见并提交                          | 后端保存到 agent_events（kind=review） |
| 3   | 查看 Agent 卡片                        | 状态显示为 `revision_requested`      |
| 4   | 确认 Agent 不会自动重新执行                  | 无新的 Agent 启动事件                  |


---

## 边界测试 (BT)


| ID    | 场景           | 操作            | 期望结果   |
| ----- | ------------ | ------------- | ------ |
| BT-01 | 大 diff 文件    | 打开较大 patch    | UI 仍可用 |
| BT-02 | 二进制文件        | diff 中出现非文本文件 | 显示占位提示 |
| BT-03 | worktree 已丢失 | 请求 diff       | 返回明确错误 |


