import {
  useRef,
  useState,
  useCallback,
  useEffect,
  type ReactNode,
} from "react";
import styles from "./DAGViewport.module.css";

export interface ViewportTransform {
  scale: number;
  translateX: number;
  translateY: number;
}

const MIN_SCALE = 0.25;
const MAX_SCALE = 2.0;
const ZOOM_SENSITIVITY = 0.002;
const PAN_BOUNDARY = 100;

interface DAGViewportProps {
  children: ReactNode;
  contentWidth: number;
  contentHeight: number;
  transform: ViewportTransform;
  onTransformChange: (t: ViewportTransform) => void;
}

export function DAGViewport({
  children,
  contentWidth,
  contentHeight,
  transform,
  onTransformChange,
}: DAGViewportProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [dragging, setDragging] = useState(false);
  const dragStart = useRef({ x: 0, y: 0, tx: 0, ty: 0 });
  const [transitioning, setTransitioning] = useState(false);

  const clampTranslate = useCallback(
    (tx: number, ty: number, scale: number): [number, number] => {
      const el = containerRef.current;
      if (!el) return [tx, ty];
      const vw = el.clientWidth;
      const vh = el.clientHeight;
      const minTx = -(contentWidth + PAN_BOUNDARY) + vw / scale;
      const maxTx = PAN_BOUNDARY;
      const minTy = -(contentHeight + PAN_BOUNDARY) + vh / scale;
      const maxTy = PAN_BOUNDARY;
      return [
        Math.max(minTx, Math.min(maxTx, tx)),
        Math.max(minTy, Math.min(maxTy, ty)),
      ];
    },
    [contentWidth, contentHeight],
  );

  const handleWheel = useCallback(
    (e: WheelEvent) => {
      e.preventDefault();
      const el = containerRef.current;
      if (!el) return;

      if (e.ctrlKey || e.metaKey) {
        // Zoom (pinch or Ctrl+wheel)
        const rect = el.getBoundingClientRect();
        const mx = e.clientX - rect.left;
        const my = e.clientY - rect.top;

        const oldScale = transform.scale;
        const delta = -e.deltaY * ZOOM_SENSITIVITY;
        const newScale = Math.max(
          MIN_SCALE,
          Math.min(MAX_SCALE, oldScale * (1 + delta)),
        );

        // Zoom-to-point: keep mouse position stable
        const dagX = mx / oldScale - transform.translateX;
        const dagY = my / oldScale - transform.translateY;
        const newTx = mx / newScale - dagX;
        const newTy = my / newScale - dagY;
        const [cx, cy] = clampTranslate(newTx, newTy, newScale);

        onTransformChange({ scale: newScale, translateX: cx, translateY: cy });
      } else {
        // Pan (two-finger scroll / regular wheel)
        const newTx = transform.translateX - e.deltaX / transform.scale;
        const newTy = transform.translateY - e.deltaY / transform.scale;
        const [cx, cy] = clampTranslate(newTx, newTy, transform.scale);
        onTransformChange({ ...transform, translateX: cx, translateY: cy });
      }
    },
    [transform, onTransformChange, clampTranslate],
  );

  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    el.addEventListener("wheel", handleWheel, { passive: false });
    return () => el.removeEventListener("wheel", handleWheel);
  }, [handleWheel]);

  const handlePointerDown = useCallback(
    (e: React.PointerEvent) => {
      // Only start drag on the viewport background, not on nodes
      if ((e.target as HTMLElement).closest("[data-dag-node]")) return;
      if (e.button !== 0) return;
      setDragging(true);
      dragStart.current = {
        x: e.clientX,
        y: e.clientY,
        tx: transform.translateX,
        ty: transform.translateY,
      };
      (e.target as HTMLElement).setPointerCapture(e.pointerId);
    },
    [transform.translateX, transform.translateY],
  );

  const handlePointerMove = useCallback(
    (e: React.PointerEvent) => {
      if (!dragging) return;
      const dx = e.clientX - dragStart.current.x;
      const dy = e.clientY - dragStart.current.y;
      const newTx = dragStart.current.tx + dx / transform.scale;
      const newTy = dragStart.current.ty + dy / transform.scale;
      const [cx, cy] = clampTranslate(newTx, newTy, transform.scale);
      onTransformChange({ ...transform, translateX: cx, translateY: cy });
    },
    [dragging, transform, onTransformChange, clampTranslate],
  );

  const handlePointerUp = useCallback(() => {
    setDragging(false);
  }, []);

  const fitToView = useCallback(() => {
    const el = containerRef.current;
    if (!el || contentWidth === 0 || contentHeight === 0) return;
    const vw = el.clientWidth;
    const vh = el.clientHeight;
    const scale = Math.min(vw / contentWidth, vh / contentHeight, 1.0) * 0.9;
    const tx = (vw / scale - contentWidth) / 2;
    const ty = (vh / scale - contentHeight) / 2;
    setTransitioning(true);
    onTransformChange({ scale, translateX: tx, translateY: ty });
    setTimeout(() => setTransitioning(false), 300);
  }, [contentWidth, contentHeight, onTransformChange]);

  const zoomBy = useCallback(
    (delta: number) => {
      const el = containerRef.current;
      if (!el) return;
      const vw = el.clientWidth;
      const vh = el.clientHeight;
      const mx = vw / 2;
      const my = vh / 2;

      const oldScale = transform.scale;
      const newScale = Math.max(
        MIN_SCALE,
        Math.min(MAX_SCALE, oldScale + delta),
      );

      const dagX = mx / oldScale - transform.translateX;
      const dagY = my / oldScale - transform.translateY;
      const newTx = mx / newScale - dagX;
      const newTy = my / newScale - dagY;
      const [cx, cy] = clampTranslate(newTx, newTy, newScale);
      onTransformChange({ scale: newScale, translateX: cx, translateY: cy });
    },
    [transform, onTransformChange, clampTranslate],
  );

  return (
    <div className={styles.wrapper}>
      <div
        ref={containerRef}
        className={`${styles.container} ${dragging ? styles.grabbing : styles.grab}`}
        onPointerDown={handlePointerDown}
        onPointerMove={handlePointerMove}
        onPointerUp={handlePointerUp}
        onPointerCancel={handlePointerUp}
      >
        <div
          className={styles.transformLayer}
          style={{
            transform: `scale(${transform.scale}) translate(${transform.translateX}px, ${transform.translateY}px)`,
            transformOrigin: "0 0",
            transition: transitioning
              ? "transform 300ms cubic-bezier(0.25, 0.46, 0.45, 0.94)"
              : "none",
          }}
        >
          {children}
        </div>
      </div>
      <ViewportControlsInternal
        scale={transform.scale}
        onZoomIn={() => zoomBy(0.1)}
        onZoomOut={() => zoomBy(-0.1)}
        onFit={fitToView}
      />
    </div>
  );
}

interface ControlsInternalProps {
  scale: number;
  onZoomIn: () => void;
  onZoomOut: () => void;
  onFit: () => void;
}

function ViewportControlsInternal({
  scale,
  onZoomIn,
  onZoomOut,
  onFit,
}: ControlsInternalProps) {
  return (
    <div className={styles.controls}>
      <button
        className={styles.controlBtn}
        onClick={onZoomOut}
        disabled={scale <= MIN_SCALE}
        title="Zoom out"
      >
        −
      </button>
      <span className={styles.scaleLabel}>
        {Math.round(scale * 100)}%
      </span>
      <button
        className={styles.controlBtn}
        onClick={onZoomIn}
        disabled={scale >= MAX_SCALE}
        title="Zoom in"
      >
        +
      </button>
      <div className={styles.divider} />
      <button
        className={styles.controlBtn}
        onClick={onFit}
        title="Fit to view"
      >
        ⊞
      </button>
    </div>
  );
}
