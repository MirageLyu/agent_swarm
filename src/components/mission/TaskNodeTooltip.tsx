/**
 * Task DAG 节点的 hover tooltip——通过 React Portal 渲染到 `document.body`，
 * 用 `position: fixed` 按 viewport 坐标定位，不被 SVG `<foreignObject>` /
 * DAG 容器 overflow 裁剪。
 *
 * 关键交互：**hit-test 在本组件内进行**，由本组件持有真实 DOM ref，
 * 通过 document 级 `mousemove` 实时判定鼠标是否落在 anchor / tooltip 矩形内
 * （含 6px padding 覆盖 8px 视觉间距），通过 `onHoverChange(inside)` 回调
 * 上报给父组件。
 *
 * 之前用 `forwardRef + useImperativeHandle` 让父组件拿 ref 做 hit-test 失败：
 * portal 子节点 + `useImperativeHandle(deps=[])` 的时机让 forwardedRef.current
 * 在父组件第一次跑 effect 时仍是 null，hit-test 永远命不中 tooltip 矩形——
 * 鼠标一离开 node 就被判定为"出界"，立即关闭。
 *
 * 定位：默认节点右侧 8px；右侧不够则左侧；都不够贴 viewport 边。
 * 垂直对齐节点顶端，底部溢出则上移。max-height ≈ min(vh-16, 540)，
 * 超过显示滚动条可悬停滚动。
 */
import {
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";
import styles from "./TaskNodeTooltip.module.css";

const MARGIN = 8;
const PREFERRED_WIDTH = 300;
const MAX_HEIGHT_CAP = 540;
const HOVER_PAD = 6;

interface TaskNodeTooltipProps {
  /** 触发 tooltip 的 anchor（通常是 node 的 getBoundingClientRect 结果）。 */
  anchor: DOMRect;
  /**
   * 每次 document mousemove 都会调用，告诉父组件鼠标当前是否仍在
   * "anchor + tooltip"（含 6px padding）的并集矩形内。
   * 父组件据此决定是否取消 / 启动 grace timer。
   */
  onHoverChange: (inside: boolean) => void;
  children: ReactNode;
}

interface ComputedPos {
  left: number;
  top: number;
  maxHeight: number;
  width: number;
}

function computeInitialPos(anchor: DOMRect): ComputedPos {
  const vw = window.innerWidth;
  const vh = window.innerHeight;
  const maxHeight = Math.min(vh - 2 * MARGIN, MAX_HEIGHT_CAP);
  const width = Math.min(PREFERRED_WIDTH, vw - 2 * MARGIN);

  let left = anchor.right + MARGIN;
  if (left + width > vw - MARGIN) {
    left = anchor.left - width - MARGIN;
  }
  if (left < MARGIN) {
    left = Math.max(MARGIN, vw - width - MARGIN);
  }

  let top = anchor.top;
  if (top < MARGIN) top = MARGIN;
  return { left, top, maxHeight, width };
}

function refinePos(initial: ComputedPos, actualHeight: number): ComputedPos {
  const vh = window.innerHeight;
  const height = Math.min(actualHeight, initial.maxHeight);
  let top = initial.top;
  if (top + height > vh - MARGIN) {
    top = Math.max(MARGIN, vh - height - MARGIN);
  }
  return { ...initial, top };
}

function pointInRect(
  x: number,
  y: number,
  r: DOMRect | undefined | null,
): boolean {
  if (!r) return false;
  return (
    x >= r.left - HOVER_PAD &&
    x <= r.right + HOVER_PAD &&
    y >= r.top - HOVER_PAD &&
    y <= r.bottom + HOVER_PAD
  );
}

export function TaskNodeTooltip({
  anchor,
  onHoverChange,
  children,
}: TaskNodeTooltipProps) {
  const innerRef = useRef<HTMLDivElement>(null);
  const initial = useMemo(() => computeInitialPos(anchor), [anchor]);
  const [pos, setPos] = useState<ComputedPos>(initial);

  useLayoutEffect(() => {
    setPos(initial);
  }, [initial]);

  useLayoutEffect(() => {
    if (!innerRef.current) return;
    const rect = innerRef.current.getBoundingClientRect();
    const refined = refinePos(initial, rect.height);
    if (refined.top !== pos.top) setPos(refined);
    // 仅依赖 initial——pos 自身变化时不应回测。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [initial]);

  // hit-test 由本组件自管：anchor 来自 props（实时），tooltip 矩形通过 own ref
  // getBoundingClientRect 拿到。鼠标在两者任一矩形内（含 padding）→ inside=true。
  useEffect(() => {
    function handleMove(e: MouseEvent) {
      const ttRect = innerRef.current?.getBoundingClientRect();
      const inside =
        pointInRect(e.clientX, e.clientY, anchor) ||
        pointInRect(e.clientX, e.clientY, ttRect);
      onHoverChange(inside);
    }
    document.addEventListener("mousemove", handleMove);
    return () => document.removeEventListener("mousemove", handleMove);
  }, [anchor, onHoverChange]);

  return createPortal(
    <div
      ref={innerRef}
      className={styles.tooltip}
      style={{
        left: pos.left,
        top: pos.top,
        maxHeight: pos.maxHeight,
        width: pos.width,
      }}
      role="tooltip"
    >
      {children}
    </div>,
    document.body,
  );
}
