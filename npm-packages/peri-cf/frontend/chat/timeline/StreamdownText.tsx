import { memo } from 'react';
import { Streamdown, type Components } from 'streamdown';
import { cn } from '@/lib';
import '../../styles/chat-markdown.css';

/** react-markdown / Streamdown 会给自定义组件注入 `node`；它不是 DOM 属性。 */
function domProps<T extends { node?: unknown }>({ node: _node, ...props }: T): Omit<T, 'node'> {
  return props;
}

/** 链接与图片共用的协议白名单；不安全协议不交给浏览器。 */
function safeHref(href: string | undefined): string {
  if (!href || /^\s*(?:javascript|vbscript|data):/i.test(href)) return '';
  return href;
}

const components: Components = {
  a: ({ className, href, children, ...props }) => {
    const safe = safeHref(href);
    if (!safe) {
      return (
        <span className={cn('aui-md-a', className)} data-blocked-href="true">
          {children}
        </span>
      );
    }
    return (
      <a
        className={cn('aui-md-a', className)}
        href={safe}
        target="_blank"
        rel="noreferrer"
        {...domProps(props)}
      >
        {children}
      </a>
    );
  },
  img: ({ className, alt, src, ...props }) => {
    // 不安全或为空的 src 不渲染，避免 React 空 src 警告与对当前页面的空请求。
    const safe = safeHref(src);
    if (!safe) return null;
    return (
      <img
        className={cn('aui-md-img', className)}
        alt={alt ?? ''}
        loading="lazy"
        src={safe}
        {...domProps(props)}
      />
    );
  },
};

export interface StreamdownTextProps {
  readonly text: string;
  readonly className?: string;
  /** 流式中允许 Streamdown 补全未闭合的 Markdown 块。 */
  readonly streaming?: boolean;
}

/**
 * 对话正文渲染：Streamdown 负责流式 Markdown（未闭合块补全、GFM、代码块与表格），
 * 这里只保留链接/图片协议白名单与既有排版类名。
 */
export const StreamdownText = memo(function StreamdownText({
  text,
  className,
  streaming = false,
}: StreamdownTextProps) {
  return (
    <div className={cn('aui-md', className)} data-slot="peri-markdown">
      <Streamdown
        mode={streaming ? 'streaming' : 'static'}
        isAnimating={streaming}
        parseIncompleteMarkdown={streaming}
        skipHtml
        lineNumbers={false}
        components={components}
      >
        {text}
      </Streamdown>
    </div>
  );
});
