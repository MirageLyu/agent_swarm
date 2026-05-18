import { useEffect, useState } from "react";
import styles from "./ThinkingIndicator.module.css";

/// 推理模型 thinking 阶段的轻量占位卡。
///
/// # 出现条件
///
/// `reasoningStartedAt !== null`（前端 onAgentStream 收到第一个 reasoning_delta
/// 时由 store 设置）。第一个 text_delta 抵达 / agent 进终态 → store 清空，本组件
/// 自动卸载。
///
/// # 信息密度
///
/// 三个信号让用户知道"agent 还在工作"：
///   1. **💭 emoji + "思考中"**：直觉信号
///   2. **滴答的秒数**：每秒 +1，让用户感知"还在动"，不是僵死
///   3. **累计字符长度**：thinking 内容多大反映模型在认真想问题；
///      短数字（< 100 字）通常代表模型只是在 framing 问题，>1k 是深度推理
interface Props {
  /// reasoningStartedAt（Date.now() 毫秒）。null = 不渲染。
  startedAt: number | null;
  /// 累计字符数。0 时只显示秒数。
  chars: number;
}

export function ThinkingIndicator({ startedAt, chars }: Props) {
  // 用 elapsed state + setInterval(1000) 而不是直接读 Date.now()，
  // 避免每次父组件渲染都强制重算。
  const [elapsed, setElapsed] = useState<number>(() =>
    startedAt ? Math.floor((Date.now() - startedAt) / 1000) : 0,
  );

  useEffect(() => {
    if (startedAt === null) return;
    setElapsed(Math.floor((Date.now() - startedAt) / 1000));
    const id = window.setInterval(() => {
      setElapsed(Math.floor((Date.now() - startedAt) / 1000));
    }, 1000);
    return () => window.clearInterval(id);
  }, [startedAt]);

  if (startedAt === null) return null;

  const charsLabel =
    chars >= 1000
      ? `${(chars / 1000).toFixed(1)}k chars`
      : `${chars} chars`;

  return (
    <div className={styles.indicator} aria-live="polite">
      <span className={styles.icon}>💭</span>
      <span className={styles.label}>Thinking</span>
      <span className={styles.dots}>
        <span className={styles.dot} />
        <span className={styles.dot} />
        <span className={styles.dot} />
      </span>
      <span className={styles.meta}>
        {elapsed}s
        {chars > 0 ? ` · ${charsLabel}` : ""}
      </span>
    </div>
  );
}
