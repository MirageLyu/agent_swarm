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

  const transformRef = useRef(transform);
  transformRef.current = transform;

  const onChangeRef = useRef(onTransformChange);
  onChangeRef.current = onTransformChange;

  const contentRef = useRef({ w: contentWidth, h: contentHeight });
  contentRef.current = { w: contentWidth, h: contentHeight };

  const clamp = useCallback(
    (tx: number, ty: number, scale: number): [number, number] => {
      const el = containerRef.current;
      if (!el) return [tx, ty];
      const vw = el.clientWidth;
      const vh = el.clientHeight;
      const { w, h } = contentRef.current;
      const rawMinTx = -(w + PAN_BOUNDARY) + vw / scale;
      const rawMaxTx = PAN_BOUNDARY;
      const rawMinTy = -(h + PAN_BOUNDARY) + vh / scale;
      const rawMaxTy = PAN_BOUNDARY;
      const loX = Math.min(rawMinTx, rawMaxTx);
      const hiX = Math.max(rawMinTx, rawMaxTx);
      const loY = Math.min(rawMinTy, rawMaxTy);
      const hiY = Math.max(rawMinTy, rawMaxTy);
      return [
        Math.max(loX, Math.min(hiX, tx)),
        Math.max(loY, Math.min(hiY, ty)),
      ];
    },
    [],
  );

  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const t = transformRef.current;
    const [cx, cy] = clamp(t.translateX, t.translateY, t.scale);
    if (cx !== t.translateX || cy !== t.translateY) {
      onChangeRef.current({ scale: t.scale, translateX: cx, translateY: cy });
    }
  }, [contentWidth, contentHeight, clamp]);

  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;

    function handleWheel(e: WheelEvent) {
      e.preventDefault();
      const t = transformRef.current;

      if (e.ctrlKey || e.metaKey) {
        const rect = el!.getBoundingClientRect();
        const mx = e.clientX - rect.left;
        const my = e.clientY - rect.top;

        const oldScale = t.scale;
        const delta = -e.deltaY * ZOOM_SENSITIVITY;
        const newScale = Math.max(
          MIN_SCALE,
          Math.min(MAX_SCALE, oldScale * (1 + delta)),
        );

        const dagX = mx / oldScale - t.translateX;
        const dagY = my / oldScale - t.translateY;
        const newTx = mx / newScale - dagX;
        const newTy = my / newScale - dagY;
        const [cx, cy] = clamp(newTx, newTy, newScale);

        onChangeRef.current({ scale: newScale, translateX: cx, translateY: cy });
      } else {
        const newTx = t.translateX - e.deltaX / t.scale;
        const newTy = t.translateY - e.deltaY / t.scale;
        const [cx, cy] = clamp(newTx, newTy, t.scale);
        onChangeRef.current({ scale: t.scale, translateX: cx, translateY: cy });
      }
    }

    el.addEventListener("wheel", handleWheel, { passive: false });
    return () => el.removeEventListener("wheel", handleWheel);
  }, [clamp]);

  const handlePointerDown = useCallback(
    (e: React.PointerEvent) => {
      if ((e.target as HTMLElement).closest("[data-dag-node]")) return;
      if (e.button !== 0) return;
      const t = transformRef.current;
      setDragging(true);
      dragStart.current = {
        x: e.clientX,
        y: e.clientY,
        tx: t.translateX,
        ty: t.translateY,
      };
      (e.target as HTMLElement).setPointerCapture(e.pointerId);
    },
    [],
  );

  const handlePointerMove = useCallback(
    (e: React.PointerEvent) => {
      if (!dragging) return;
      const t = transformRef.current;
      const dx = e.clientX - dragStart.current.x;
      const dy = e.clientY - dragStart.current.y;
      const newTx = dragStart.current.tx + dx / t.scale;
      const newTy = dragStart.current.ty + dy / t.scale;
      const [cx, cy] = clamp(newTx, newTy, t.scale);
      onChangeRef.current({ scale: t.scale, translateX: cx, translateY: cy });
    },
    [dragging, clamp],
  );

  const handlePointerUp = useCallback(() => {
    setDragging(false);
  }, []);

  const fitToView = useCallback(() => {
    const el = containerRef.current;
    const { w, h } = contentRef.current;
    if (!el || w === 0 || h === 0) return;
    const vw = el.clientWidth;
    const vh = el.clientHeight;
    const scale = Math.min(vw / w, vh / h, 1.0) * 0.9;
    const tx = (vw / scale - w) / 2;
    const ty = (vh / scale - h) / 2;
    setTransitioning(true);
    onChangeRef.current({ scale, translateX: tx, translateY: ty });
    setTimeout(() => setTransitioning(false), 300);
  }, []);

  const zoomBy = useCallback(
    (delta: number) => {
      const el = containerRef.current;
      if (!el) return;
      const vw = el.clientWidth;
      const vh = el.clientHeight;
      const mx = vw / 2;
      const my = vh / 2;
      const t = transformRef.current;

      const newScale = Math.max(
        MIN_SCALE,
        Math.min(MAX_SCALE, t.scale + delta),
      );

      const dagX = mx / t.scale - t.translateX;
      const dagY = my / t.scale - t.translateY;
      const newTx = mx / newScale - dagX;
      const newTy = my / newScale - dagY;
      const [cx, cy] = clamp(newTx, newTy, newScale);
      onChangeRef.current({ scale: newScale, translateX: cx, translateY: cy });
    },
    [clamp],
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
