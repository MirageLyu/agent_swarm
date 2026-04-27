/**
 * App 级别的 planner 事件订阅，把 `planner-stream`（raw token）和
 * `planner-step`（agent loop 步骤）事件持续累积到全局 store。
 *
 * 必须挂在 App 根，**不能挂在 MissionsView**——否则用户在 plan 进行中切到
 * 其他 view 时 MissionsView unmount，订阅被 cleanup 解绑，期间所有事件全丢；
 * 切回来后只能看到"切回之后"的部分，旧 thinking / agent loop 历史完全消失。
 *
 * 职责分工：
 * - planner-stream 事件 → 累积到 `plannerStream.text`（thinking 文本）。
 * - planner-step 事件 → 第一次见到 session_id 就写入 `livePlannerSessionId`，
 *   让任何后续 mount 的 PlannerLoopPanel 都能用 `listPlannerSteps` 拉历史。
 * - step 自身的内容仍由 PlannerLoopPanel 自己订阅渲染（保持原有逻辑）。
 */
import { useEffect } from "react";
import { onPlannerStep, onPlannerStream } from "../ipc/events";
import { useTaskStore } from "../stores/task-store";

export function usePlannerEventSync() {
  useEffect(() => {
    const unsubStreamP = onPlannerStream((payload) => {
      const { setPlannerStream } = useTaskStore.getState();
      if (payload.kind === "reasoning_delta" || payload.kind === "text_delta") {
        setPlannerStream((s) => ({
          ...s,
          text: s.text + payload.content,
          tokenCount: s.tokenCount + 1,
        }));
      } else if (payload.kind === "done") {
        setPlannerStream((s) => ({
          ...s,
          status: s.status === "cancelled" ? s.status : "done",
          elapsedMs: s.startTime ? Date.now() - s.startTime : s.elapsedMs,
        }));
      } else if (payload.kind === "error") {
        setPlannerStream((s) => ({
          ...s,
          status: s.status === "cancelled" ? s.status : "error",
          errorMessage: payload.content,
          elapsedMs: s.startTime ? Date.now() - s.startTime : s.elapsedMs,
        }));
      }
    });

    const unsubStepP = onPlannerStep((payload) => {
      const { livePlannerSessionId, setLivePlannerSessionId } =
        useTaskStore.getState();
      // 第一次见到 session_id 就锁定——切回来 PlannerLoopPanel mount 时即可
      // 通过 store 拿到 sessionId 并 listPlannerSteps 拉完整历史。
      if (!livePlannerSessionId) {
        setLivePlannerSessionId(payload.session_id);
      }
    });

    return () => {
      unsubStreamP.then((fn) => fn());
      unsubStepP.then((fn) => fn());
    };
  }, []);
}
