import { Component, type ErrorInfo, type ReactNode } from "react";

interface OpenUIErrorBoundaryProps {
  readonly children: ReactNode;
  /** 渲染异常时展示的回退内容（通常是原始 Markdown）。 */
  readonly fallback: ReactNode;
  /** 程序变化时重置错误态；由 OpenUIMessage 传 program。 */
  readonly resetKey?: string;
  readonly onError?: (error: Error) => void;
}

interface OpenUIErrorBoundaryState {
  readonly failed: boolean;
}

/**
 * Renderer / 懒加载 chunk 的异常兜底。
 * 失败时不丢弃模型输出：调用方把原文 Markdown 作为 fallback 传进来。
 */
export class OpenUIErrorBoundary extends Component<OpenUIErrorBoundaryProps, OpenUIErrorBoundaryState> {
  state: OpenUIErrorBoundaryState = { failed: false };

  static getDerivedStateFromError(): OpenUIErrorBoundaryState {
    return { failed: true };
  }

  componentDidCatch(error: Error, _info: ErrorInfo): void {
    this.props.onError?.(error);
  }

  componentDidUpdate(previous: OpenUIErrorBoundaryProps): void {
    if (this.state.failed && previous.resetKey !== this.props.resetKey) {
      this.setState({ failed: false });
    }
  }

  render(): ReactNode {
    if (!this.state.failed) return this.props.children;
    return <div data-openui-fallback="render-error">{this.props.fallback}</div>;
  }
}
