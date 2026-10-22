import { isValidElement, memo, type ReactNode } from 'react';
import ReactMarkdown, { type Components } from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { Check, Copy } from 'lucide-react';
import { cn } from '@/lib';
import { useCopyToClipboard } from '../../hooks';
import '../../styles/chat-markdown.css';

function textOf(node: ReactNode): string {
  if (typeof node === 'string' || typeof node === 'number') return String(node);
  if (Array.isArray(node)) return node.map(textOf).join('');
  if (isValidElement<{ children?: ReactNode }>(node)) return textOf(node.props.children);
  return '';
}

function codeInfo(children: ReactNode): { language: string; code: string } {
  if (isValidElement<{ className?: string; children?: ReactNode }>(children)) {
    const className = children.props.className ?? '';
    return {
      language: /language-([\w-]+)/.exec(className)?.[1] ?? '',
      code: textOf(children.props.children),
    };
  }
  return { language: '', code: textOf(children) };
}

/** 代码块：header 与 pre 同级，复用 `chat-markdown.css` 的拼接外观。 */
function PreBlock({ children }: { children?: ReactNode }) {
  const { language, code } = codeInfo(children);
  const { isCopied, copyToClipboard } = useCopyToClipboard();
  return (
    <>
      <div className="aui-code-header-root">
        <span className="aui-code-header-language">{language || 'code'}</span>
        <button
          type="button"
          className="text-muted-foreground hover:text-foreground inline-flex size-6 items-center justify-center rounded-md transition-colors"
          aria-label="复制代码"
          onClick={() => copyToClipboard(code)}
        >
          {isCopied ? <Check className="size-3.5" /> : <Copy className="size-3.5" />}
        </button>
      </div>
      <pre>{children}</pre>
    </>
  );
}

/** react-markdown 会给自定义组件注入 `node`；它不是 DOM 属性，必须剔除。 */
function domProps<T extends { node?: unknown }>({ node: _node, ...props }: T): Omit<T, 'node'> {
  return props;
}

/** 链接协议白名单；不安全协议保持为空 href，不把可执行 URL 交给浏览器。 */
function safeHref(href: string | undefined): string {
  if (!href || /^\s*(?:javascript|vbscript|data):/i.test(href)) return '';
  return href;
}

const components: Components = {
  h1: ({ className, ...props }) => <h1 className={cn('aui-md-h1', className)} {...domProps(props)} />,
  h2: ({ className, ...props }) => <h2 className={cn('aui-md-h2', className)} {...domProps(props)} />,
  h3: ({ className, ...props }) => <h3 className={cn('aui-md-h3', className)} {...domProps(props)} />,
  h4: ({ className, ...props }) => <h4 className={cn('aui-md-h4', className)} {...domProps(props)} />,
  h5: ({ className, ...props }) => <h5 className={cn('aui-md-h5', className)} {...domProps(props)} />,
  h6: ({ className, ...props }) => <h6 className={cn('aui-md-h6', className)} {...domProps(props)} />,
  p: ({ className, ...props }) => <p className={cn('aui-md-p', className)} {...domProps(props)} />,
  a: ({ className, href, ...props }) => (
    <a
      className={cn('aui-md-a', className)}
      href={safeHref(href)}
      target="_blank"
      rel="noreferrer"
      {...domProps(props)}
    />
  ),
  blockquote: ({ className, ...props }) => <blockquote className={cn('aui-md-blockquote', className)} {...domProps(props)} />,
  ul: ({ className, ...props }) => <ul className={cn('aui-md-ul', className)} {...domProps(props)} />,
  ol: ({ className, ...props }) => <ol className={cn('aui-md-ol', className)} {...domProps(props)} />,
  hr: ({ className, ...props }) => <hr className={cn('aui-md-hr', className)} {...domProps(props)} />,
  table: ({ className, ...props }) => (
    <div className="aui-md-table-wrapper">
      <table className={cn('aui-md-table', className)} {...domProps(props)} />
    </div>
  ),
  th: ({ className, ...props }) => <th className={cn('aui-md-th', className)} {...domProps(props)} />,
  td: ({ className, ...props }) => <td className={cn('aui-md-td', className)} {...domProps(props)} />,
  img: ({ className, alt, src, ...props }) => {
    // 不安全或为空的 src 不渲染，避免 React 空 src 警告与对当前页面的空请求。
    const safeSrc = safeHref(src);
    if (!safeSrc) return null;
    return (
      <img
        className={cn('aui-md-img', className)}
        alt={alt ?? ''}
        loading="lazy"
        src={safeSrc}
        {...domProps(props)}
      />
    );
  },
  pre: PreBlock,
};

export interface MarkdownTextProps {
  text: string;
  className?: string;
}

/** 自有 Markdown 渲染：不依赖 assistant-ui runtime，复用现有排版样式。 */
export const MarkdownText = memo(function MarkdownText({ text, className }: MarkdownTextProps) {
  return (
    <div className={cn('aui-md', className)} data-slot="peri-markdown">
      <ReactMarkdown remarkPlugins={[remarkGfm]} components={components}>
        {text}
      </ReactMarkdown>
    </div>
  );
});
