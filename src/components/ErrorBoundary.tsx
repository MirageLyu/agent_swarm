/**
 * 全局 ErrorBoundary：兜住任何 view / 子组件抛出的 render 或 effect 错误，
 * 避免一处异常导致整个 React tree unmount 出现白屏。
 *
 * 使用方式：在 `<ActiveView />` 等可疑节点外面包一层。
 *
 * 设计要点：
 * - 同时捕获 render 阶段与 effect 阶段的同步错误（React 17+）。
 * - 提供 "Reload" 按钮，重置 boundary 状态后重新挂载子树；
 *   如果错误依然发生，再次回落到错误页（不会无限刷新）。
 * - dev 模式下展示 error message + stack，方便排障。
 * - 使用纯内联样式，不依赖 CSS modules，确保即便样式管线坏掉也能渲染。
 */
import { Component, type ErrorInfo, type ReactNode } from "react";

interface Props {
  children: ReactNode;
  /** 显示在标题位置的 scope，例如 "Active view"，方便定位是哪一块挂了。 */
  scope?: string;
}

interface State {
  error: Error | null;
  info: ErrorInfo | null;
}

export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null, info: null };

  static getDerivedStateFromError(error: Error): Partial<State> {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    this.setState({ info });
    // eslint-disable-next-line no-console
    console.error("[ErrorBoundary]", this.props.scope ?? "(unknown scope)", error, info);
  }

  private handleReset = () => {
    this.setState({ error: null, info: null });
  };

  private handleReload = () => {
    window.location.reload();
  };

  render() {
    if (this.state.error) {
      const isDev = import.meta.env.DEV;
      return (
        <div
          role="alert"
          style={{
            padding: "32px",
            maxWidth: 720,
            margin: "48px auto",
            color: "var(--color-text-primary, #1d1d1f)",
            background: "var(--color-bg-elevated, #fff)",
            border: "1px solid var(--color-border, #e0e0e0)",
            borderRadius: 12,
            boxShadow: "0 4px 24px rgba(0,0,0,0.06)",
            fontFamily: "system-ui, -apple-system, BlinkMacSystemFont, sans-serif",
          }}
        >
          <h2 style={{ margin: "0 0 8px", fontSize: 18, fontWeight: 600 }}>
            Something broke{this.props.scope ? ` in ${this.props.scope}` : ""}.
          </h2>
          <p style={{ margin: "0 0 16px", color: "var(--color-text-secondary, #555)", fontSize: 13 }}>
            The UI hit an unexpected error. You can try resetting this section or reloading
            the whole window. If the error persists, copy the details below and report it.
          </p>
          <div style={{ display: "flex", gap: 8, marginBottom: 16 }}>
            <button
              type="button"
              onClick={this.handleReset}
              style={{
                padding: "6px 14px",
                fontSize: 13,
                borderRadius: 6,
                border: "1px solid var(--color-border, #d0d0d0)",
                background: "var(--color-bg-elevated, #fff)",
                cursor: "pointer",
              }}
            >
              Try again
            </button>
            <button
              type="button"
              onClick={this.handleReload}
              style={{
                padding: "6px 14px",
                fontSize: 13,
                borderRadius: 6,
                border: "1px solid var(--color-accent, #0a84ff)",
                background: "var(--color-accent, #0a84ff)",
                color: "#fff",
                cursor: "pointer",
              }}
            >
              Reload window
            </button>
          </div>
          {isDev && (
            <details style={{ fontSize: 12 }}>
              <summary style={{ cursor: "pointer", color: "var(--color-text-secondary, #666)" }}>
                Error details (dev only)
              </summary>
              <pre
                style={{
                  whiteSpace: "pre-wrap",
                  wordBreak: "break-word",
                  background: "var(--color-bg-secondary, #f5f5f7)",
                  padding: 12,
                  marginTop: 8,
                  borderRadius: 6,
                  maxHeight: 360,
                  overflow: "auto",
                  fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
                }}
              >
{String(this.state.error?.stack ?? this.state.error?.message ?? this.state.error)}
{this.state.info?.componentStack ? `\n\nComponent stack:${this.state.info.componentStack}` : ""}
              </pre>
            </details>
          )}
        </div>
      );
    }
    return this.props.children;
  }
}
