# FM-07: Mission Planning UX Enhancements — 测试用例

> 版本: v1.0 | 日期: 2026-04-02

---

## 单元测试 (UT)

### UT-01: 画布变换计算（TypeScript）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|---------|
| UT-01.1 | 初始状态 | 无操作 | scale=1, translateX=0, translateY=0 |
| UT-01.2 | Zoom in (中心) | 视口中心 wheel 缩放 +10% | scale=1.1, 内容中心位置不变 |
| UT-01.3 | Zoom to point | 在 (100, 50) 处 wheel 缩放 ×1.2 | (100, 50) 处的 DAG 内容点在变换后仍映射到视口 (100, 50) |
| UT-01.4 | Zoom 下界 | 连续缩小到 scale < 0.25 | scale 被 clamp 为 0.25 |
| UT-01.5 | Zoom 上界 | 连续放大到 scale > 2.0 | scale 被 clamp 为 2.0 |
| UT-01.6 | Fit to view | DAG 尺寸 1000×500, viewport 800×400 | scale = min(800/1000, 400/500) × 0.9 = 0.72, 内容居中 |
| UT-01.7 | Fit to view (小 DAG) | DAG 尺寸 200×100, viewport 800×400 | scale = 1.0 (不放大超过 100%)，内容居中 |
| UT-01.8 | Pan 平移 | 拖拽 dx=50, dy=-30 | translateX += 50/scale, translateY -= 30/scale |
| UT-01.9 | Pan 边界限制 | 大幅平移超出内容范围 | translate 被 clamp 到内容边界 ± 100px |
| UT-01.10 | Zoom 后 Pan | scale=0.5 时拖拽 dx=100 | translateX += 100/0.5 = 200 (DAG 坐标) |

### UT-02: Planner 流式文本聚合（Rust）

| ID | 场景 | 输入 | 期望结果 |
|----|------|------|---------|
| UT-02.1 | 正常流式聚合 | 多个 text_delta 片段 | 聚合为完整 JSON 字符串，`parse_and_validate` 成功 |
| UT-02.2 | 含 reasoning_content | 流式响应含 `reasoning_content` delta 和 `content` delta | reasoning 和 content 分别收集，`planner-stream` 分别以 `reasoning_delta` 和 `text_delta` 推送 |
| UT-02.3 | 流式超时 | 90 秒内无 message_stop | 返回超时错误，前端收到 `error` 事件 |
| UT-02.4 | 流式后 JSON 非法 | 流式完成但内容不是有效 JSON | 触发重试（附错误信息），重试仍用流式 |
| UT-02.5 | 空 content | 流式完成但 content 为空 | 返回 `EmptyTaskList` 错误 |

### UT-03: PlannerStreamPanel 组件（React）

| ID | 场景 | 操作 | 期望结果 |
|----|------|------|---------|
| UT-03.1 | 初始隐藏 | 未开始 planning | 面板不可见 |
| UT-03.2 | Planning 时显示 | 收到第一个 text_delta | 面板展开，显示文本 |
| UT-03.3 | 自动滚动 | 持续接收 delta | 面板自动滚动到底部 |
| UT-03.4 | 完成后折叠 | 收到 done 事件 | 面板自动折叠，可手动展开 |
| UT-03.5 | 错误时显示 | 收到 error 事件 | 面板显示错误文本，红色样式 |
| UT-03.6 | 计时器 | 开始后 5 秒 | 显示 "5s" 计时 |

### UT-04: ViewportControls 组件（React）

| ID | 场景 | 操作 | 期望结果 |
|----|------|------|---------|
| UT-04.1 | 显示当前缩放 | scale=1.0 | 显示 "100%" |
| UT-04.2 | 点击 + | 点击放大按钮 | scale 增加 10%，显示更新 |
| UT-04.3 | 点击 - | 点击缩小按钮 | scale 减少 10%，显示更新 |
| UT-04.4 | 点击 Fit | 点击 Fit 按钮 | 触发 fit-to-view 回调 |
| UT-04.5 | 边界禁用 | scale=2.0 | + 按钮 disabled |
| UT-04.6 | 边界禁用 | scale=0.25 | - 按钮 disabled |

---

## 集成测试 (IT)

### IT-01: DAG 画布交互端到端

**前置条件**: 已有一个包含 5+ 节点的 Mission DAG

| 步骤 | 操作 | 期望结果 |
|:---:|------|---------|
| 1 | 打开 Mission，查看 DAG | DAG 正常渲染，控制栏可见，显示 "100%" |
| 2 | 触控板捏合放大 | DAG 平滑放大，控制栏百分比增加，内容不跳跃 |
| 3 | 触控板双指向右滑动 | DAG 画布向右平移，光标显示 grab 状态 |
| 4 | 使用鼠标滚轮 + Cmd 缩小 | DAG 缩小，以鼠标位置为中心 |
| 5 | 点击控制栏 "Fit" | DAG 带过渡动画缩放到适配视口，所有节点可见 |
| 6 | 在缩放状态下点击节点 | 节点菜单正常弹出，位置准确（不因缩放偏移） |
| 7 | 在缩放状态下编辑/删除节点 | 操作正常，DAG 重新渲染后保持当前 scale/translate |

### IT-02: Planning 流式输出端到端

**前置条件**: 应用已启动，LLM API 可用

| 步骤 | 操作 | 期望结果 |
|:---:|------|---------|
| 1 | 输入需求 "实现一个完整的电商购物车功能" | 输入正常，Plan 按钮可用 |
| 2 | 点击 Plan Mission | 按钮变为 loading，流式输出面板展开 |
| 3 | 等待 2-3 秒 | 面板开始显示文本（推理模型的 reasoning 内容），计时器显示已用时间 |
| 4 | 观察流式输出 | 文本持续追加，面板自动滚动，可看到 LLM 的思考过程 |
| 5 | 等待 LLM 完成 | 流式面板自动折叠，DAG 渲染出来，左侧列表更新 |
| 6 | 手动展开流式面板 | 可查看完整的 LLM 输出历史 |
| 7 | 再次 Plan 新 Mission | 旧的流式输出被清空，新的流式输出开始 |

### IT-03: Planning 流式取消

| 步骤 | 操作 | 期望结果 |
|:---:|------|---------|
| 1 | 输入需求，点击 Plan | 流式输出开始 |
| 2 | 在流式输出过程中点击 Cancel | 按钮恢复可用，流式面板显示 "Cancelled"，无 DAG 渲染 |
| 3 | 再次 Plan 新需求 | 正常工作，无残留状态 |

---

## 边界测试 (BT)

| ID | 场景 | 操作 | 期望结果 |
|----|------|------|---------|
| BT-01 | 超高缩放后交互 | scale=2.0 时点击节点 | 菜单位置正确，不偏移 |
| BT-02 | 超低缩放后交互 | scale=0.25 时点击节点 | 菜单位置正确，文字仍可读 |
| BT-03 | 快速连续缩放 | 触控板快速捏合多次 | 无卡顿、无跳跃，变换流畅 |
| BT-04 | 缩放+平移组合 | 先缩放到 0.5，再平移，再缩放到 1.5 | 所有变换正确累积，Fit 可恢复 |
| BT-05 | 空 DAG 缩放 | 无任务时操作画布 | 控制栏可见但 +/-/Fit 合理禁用或无效果 |
| BT-06 | 流式超时 | LLM 30 秒无输出 | 面板显示 "Connection may be slow..." |
| BT-07 | 流式输出极长 | LLM reasoning 输出 2000+ token | 面板保持 200px 高度，滚动流畅 |
| BT-08 | 浏览器缩放冲突 | Cmd+滚轮 时 | DAG 缩放，浏览器不缩放（`preventDefault`） |
| BT-09 | 窗口 resize | 缩放状态下调整窗口大小 | 画布自适应，不错位 |
| BT-10 | 流式输出后 App 重启 | 流式完成、DAG 渲染后重启 | Mission 和 Tasks 从数据库恢复，DAG 正常（流式输出不需恢复） |
