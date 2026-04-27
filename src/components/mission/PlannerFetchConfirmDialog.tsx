/**
 * FM-15 v2.2 (S3-6): Planner `fetch_url` 用户确认弹窗。
 *
 * 监听后端 `planner-fetch-confirmation` 事件，对每个 request 弹一次窗，让用户在
 * `allow_once` / `allow_session` / `deny` 三个选项中决策；用户选择后通过
 * `commands.confirmPlannerFetch` 回执给后端。
 *
 * 设计要点：
 * - 同一时刻可能有多个 request（理论上 PlannerEngine 单线程，但保险起见用队列）。
 * - 弹窗关闭（Esc / 点击遮罩）等同 `deny`，避免后端永远等待。
 * - allow_session：当前 planner session 内同 host 不再询问；allow_once：一次性。
 */
import { useEffect, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { Button } from "../ui";
import { commands, type FetchDecision } from "../../ipc/commands";
import {
  onPlannerFetchConfirmation,
  type PlannerFetchConfirmationPayload,
} from "../../ipc/events";
import styles from "./PlannerFetchConfirmDialog.module.css";

export function PlannerFetchConfirmDialog() {
  const [queue, setQueue] = useState<PlannerFetchConfirmationPayload[]>([]);
  const [pending, setPending] = useState(false);

  useEffect(() => {
    const unsubP = onPlannerFetchConfirmation((payload) => {
      setQueue((q) => [...q, payload]);
    });
    return () => {
      unsubP.then((fn) => fn());
    };
  }, []);

  const current = queue[0] ?? null;

  const respond = async (decision: FetchDecision) => {
    if (!current || pending) return;
    setPending(true);
    try {
      await commands.confirmPlannerFetch({
        request_id: current.request_id,
        decision,
      });
    } catch (e) {
      console.warn("confirmPlannerFetch failed:", e);
    } finally {
      setQueue((q) => q.slice(1));
      setPending(false);
    }
  };

  if (!current) return null;

  return (
    <Dialog.Root
      open={!!current}
      onOpenChange={(v) => {
        // 关闭/遮罩点击 = deny，确保后端不会卡住
        if (!v && !pending) void respond("deny");
      }}
    >
      <Dialog.Portal>
        <Dialog.Overlay className={styles.overlay} />
        <Dialog.Content className={styles.content}>
          <Dialog.Title className={styles.title}>
            Planner wants to fetch a URL
          </Dialog.Title>
          <Dialog.Description className={styles.description}>
            The planner is asking for permission to read this remote URL during
            planning. It will only be used as grounding context, not stored
            beyond this session.
          </Dialog.Description>

          <dl className={styles.metaList}>
            <div className={styles.metaRow}>
              <dt>URL</dt>
              <dd className={styles.urlValue} title={current.url}>
                {current.url}
              </dd>
            </div>
            <div className={styles.metaRow}>
              <dt>Host</dt>
              <dd>
                <code className={styles.hostValue}>{current.host}</code>
              </dd>
            </div>
            {current.reason?.trim() && (
              <div className={styles.metaRow}>
                <dt>Reason</dt>
                <dd className={styles.reasonValue}>{current.reason}</dd>
              </div>
            )}
          </dl>

          <p className={styles.hint}>
            <strong>Allow once</strong>: this request only.
            <br />
            <strong>Allow this session</strong>: skip future prompts for{" "}
            <code className={styles.hostValue}>{current.host}</code> in the
            current planner session.
          </p>

          {queue.length > 1 && (
            <p className={styles.queueInfo}>
              {queue.length - 1} more request(s) waiting after this one.
            </p>
          )}

          <div className={styles.actions}>
            <Button
              variant="secondary"
              size="sm"
              onClick={() => respond("deny")}
              disabled={pending}
            >
              Deny
            </Button>
            <Button
              variant="secondary"
              size="sm"
              onClick={() => respond("allow_session")}
              disabled={pending}
            >
              Allow this session
            </Button>
            <Button
              variant="primary"
              size="sm"
              onClick={() => respond("allow_once")}
              disabled={pending}
            >
              Allow once
            </Button>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
