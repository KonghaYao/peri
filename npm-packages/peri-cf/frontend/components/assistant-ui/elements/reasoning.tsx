"use client";

// 本地化定制（上游同步时逐项保留）：
// 1. 折叠行对齐 ChatGPT 观感——去掉 Brain 图标，文案中文化（思考中 / 已思考 N 秒 / 思考过程）。
// 2. 根节点底距 mb-4 → mb-3；折叠行使用 24px 过程行骨架（16px 图标盒 + 6px 间距）。
// 3. 折叠行不再随流式自动展开：默认折叠，运行中在行内显示最新完成段落首行。

import "@/styles/chat-tools.css";

import {
  createContext,
  forwardRef,
  useCallback,
  useContext,
  useEffect,
  useRef,
  useState,
} from "react";
import { useAuiState } from "@assistant-ui/react";
import { cva, type VariantProps } from "class-variance-authority";
import { ChevronDownIcon } from "lucide-react";
import { cn } from "@/lib";
import {
  CollapsibleRoot as Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "./collapsible-root";
import {
  processChevron,
  processLeading,
  processRow,
  processTitle,
} from "./surfaces";

export const ANIMATION_DURATION = 200;

type ReasoningPreviewState = {
  /** 展开面板是否处于流式预览态（用于滚动跟随与底部渐隐）。 */
  preview: boolean;
  /** 折叠行显示的首行摘要。 */
  summary: string;
};

const ReasoningPreviewContext = createContext<ReasoningPreviewState>({
  preview: false,
  summary: "",
});

type ReasoningPartLike = { type: string; text?: string };

/** assistant-ui 只把相邻 reasoning part 归入同一 group；其它 part 断开分组。 */
function reasoningGroupTexts(
  parts: readonly ReasoningPartLike[],
): string[] {
  const groups: string[] = [];
  let current: string | null = null;
  for (const part of parts) {
    if (part.type === "reasoning") {
      const text = typeof part.text === "string" ? part.text : "";
      current = current === null ? text : `${current}\n\n${text}`;
    } else if (current !== null) {
      groups.push(current);
      current = null;
    }
  }
  if (current !== null) groups.push(current);
  return groups;
}

function firstLine(text: string): string {
  const newline = text.indexOf("\n");
  return (newline === -1 ? text : text.slice(0, newline)).trim();
}

/**
 * 流式期间仅取已完成的段落：最后一段若还没有换行，视为仍在生成。
 * 段落首行已完成时即可作为摘要，与参考实现一致。
 */
function latestCompletedParagraphFirstLine(text: string): string {
  const paragraphs = text.split(/\r?\n[ \t]*\r?\n/);
  for (let index = paragraphs.length - 1; index >= 0; index -= 1) {
    const paragraph = paragraphs[index] ?? "";
    if (index === paragraphs.length - 1 && !paragraph.includes("\n")) {
      continue;
    }
    const line = firstLine(paragraph);
    if (line !== "") return line;
  }
  return "";
}

/** 摘要不保留 Markdown 的强调标记，折叠行保持纯文本。 */
const summaryOf = (text: string) => text.replaceAll("**", "");

const reasoningVariants = cva("aui-reasoning-root mb-3 w-full", {
  variants: {
    variant: {
      outline: "rounded-lg border px-3 py-2",
      ghost: "",
      muted: "bg-muted/50 rounded-lg px-3 py-2",
    },
  },
  defaultVariants: {
    variant: "outline",
  },
});

export type ReasoningRootProps = Omit<
  React.ComponentProps<typeof Collapsible>,
  "open" | "onOpenChange"
> &
  VariantProps<typeof reasoningVariants> & {
    open?: boolean;
    onOpenChange?: (open: boolean) => void;
    defaultOpen?: boolean;
    /**
     * Whether the reasoning is currently streaming. The disclosure stays
     * collapsed by default; while streaming the collapsed row shows the
     * latest completed paragraph's first line, and expanding it keeps the
     * live text bottom-pinned.
     */
    streaming?: boolean;
    /** Called right before the disclosure animates, on toggle. */
    onAnimationStart?: () => void;
  };

const ReasoningRoot = forwardRef<
  HTMLDivElement,
  Omit<ReasoningRootProps, "ref">
>(function ReasoningRoot(
  {
    className,
    variant,
    open: controlledOpen,
    onOpenChange: controlledOnOpenChange,
    defaultOpen = false,
    streaming,
    onAnimationStart,
    children,
    ...props
  },
  ref,
) {
  const [initialOpen] = useState(defaultOpen);
  const [userOpen, setUserOpen] = useState<boolean | null>(null);
  const rootRef = useRef<HTMLDivElement | null>(null);
  const [groupIndex, setGroupIndex] = useState<number | null>(null);

  const isControlled = controlledOpen !== undefined;
  const isOpen = isControlled ? controlledOpen : (userOpen ?? initialOpen);
  const isPreview = streaming === true && isOpen;

  // The group's rendered children are supplied as opaque render props, so the
  // raw reasoning text is read from the message parts instead. The DOM ordinal
  // among sibling reasoning roots maps this disclosure to its group.
  useEffect(() => {
    const element = rootRef.current;
    const parent = element?.parentElement;
    if (!element || !parent) return;
    const index = Array.from(parent.children)
      .filter((child) => child.getAttribute("data-slot") === "reasoning-root")
      .indexOf(element);
    setGroupIndex(index === -1 ? null : index);
  }, []);

  const groupText = useAuiState((state) =>
    groupIndex === null
      ? ""
      : (reasoningGroupTexts(state.message.parts)[groupIndex] ?? ""),
  );
  const summary = summaryOf(
    streaming === true
      ? latestCompletedParagraphFirstLine(groupText)
      : firstLine(groupText),
  );

  const setRootRef = useCallback(
    (node: HTMLDivElement | null) => {
      rootRef.current = node;
      if (typeof ref === "function") {
        ref(node);
      } else if (ref) {
        (ref as { current: HTMLDivElement | null }).current = node;
      }
    },
    [ref],
  );

  const handleOpenChange = useCallback(
    (open: boolean) => {
      onAnimationStart?.();
      if (!isControlled) {
        setUserOpen(open);
      }
      controlledOnOpenChange?.(open);
    },
    [onAnimationStart, isControlled, controlledOnOpenChange],
  );

  return (
    <Collapsible
      ref={setRootRef}
      data-slot="reasoning-root"
      data-variant={variant}
      open={isOpen}
      onOpenChange={handleOpenChange}
      className={cn(
        "group/reasoning-root",
        reasoningVariants({ variant, className }),
      )}
      style={
        {
          "--animation-duration": `${ANIMATION_DURATION}ms`,
        } as React.CSSProperties
      }
      {...props}
    >
      <ReasoningPreviewContext.Provider value={{ preview: isPreview, summary }}>
        {children}
      </ReasoningPreviewContext.Provider>
    </Collapsible>
  );
});

function ReasoningFade({
  side = "bottom",
  className,
  ...props
}: React.ComponentProps<"div"> & { side?: "top" | "bottom" }) {
  if (side === "top") {
    return (
      <div
        data-slot="reasoning-fade"
        className={cn(
          "aui-reasoning-fade pointer-events-none absolute inset-x-0 top-0 z-10 h-8",
          "bg-[linear-gradient(to_bottom,var(--color-background),transparent)]",
          "group-data-[variant=muted]/reasoning-root:bg-[linear-gradient(to_bottom,color-mix(in_oklab,var(--color-muted)_50%,var(--color-background)),transparent)]",
          "fade-in-0 animate-in motion-reduce:animate-none",
          "animation-duration-(--animation-duration)",
          className,
        )}
        {...props}
      />
    );
  }

  return (
    <div
      data-slot="reasoning-fade"
      className={cn(
        "aui-reasoning-fade pointer-events-none absolute inset-x-0 bottom-0 z-10 h-8",
        "bg-[linear-gradient(to_top,var(--color-background),transparent)]",
        "group-data-[variant=muted]/reasoning-root:bg-[linear-gradient(to_top,color-mix(in_oklab,var(--color-muted)_50%,var(--color-background)),transparent)]",
        "fade-in-0 animate-in motion-reduce:animate-none",
        "animation-duration-(--animation-duration)",
        className,
      )}
      {...props}
    />
  );
}

function ReasoningTrigger({
  active,
  duration,
  className,
  ...props
}: React.ComponentProps<typeof CollapsibleTrigger> & {
  active?: boolean;
  duration?: number | undefined;
}) {
  const { summary } = useContext(ReasoningPreviewContext);
  const label = active
    ? "思考中"
    : duration
      ? `已思考 ${duration} 秒`
      : "思考过程";

  return (
    <CollapsibleTrigger
      data-slot="reasoning-trigger"
      data-expandable="true"
      className={cn(
        "aui-reasoning-trigger chat-reasoning-trigger group/trigger",
        processRow,
        className,
      )}
      {...props}
    >
      <span
        aria-hidden="true"
        className={cn(processLeading, "chat-reasoning-leading")}
      >
        <ChevronDownIcon
          data-slot="reasoning-trigger-chevron"
          className={cn(processChevron, "chat-reasoning-chevron")}
        />
      </span>
      <span
        className={cn(
          "chat-reasoning-summary",
          active && "shimmer motion-reduce:animate-none",
        )}
      >
        <span
          data-slot="reasoning-trigger-label"
          className={cn(
            "aui-reasoning-trigger-label-wrapper chat-reasoning-title",
            processTitle,
            "tabular-nums",
          )}
        >
          {label}
        </span>
        {summary ? (
          <span
            data-slot="reasoning-trigger-preview"
            className="chat-reasoning-preview"
          >
            {summary}
          </span>
        ) : null}
      </span>
    </CollapsibleTrigger>
  );
}

function ReasoningContent({
  className,
  children,
  ...props
}: React.ComponentProps<typeof CollapsibleContent>) {
  const { preview: isPreview } = useContext(ReasoningPreviewContext);

  return (
    <CollapsibleContent
      data-slot="reasoning-content"
      className={cn(
        "aui-reasoning-content chat-reasoning-content relative overflow-hidden text-sm outline-none",
        "group/collapsible-content ease-[cubic-bezier(0.32,0.72,0,1)] motion-reduce:animate-none",
        "data-closed:animate-collapsible-up",
        "data-open:animate-collapsible-down",
        "data-closed:fill-mode-forwards",
        "data-closed:pointer-events-none",
        "[--tw-duration:var(--animation-duration)]",
        className,
      )}
      {...props}
    >
      <ReasoningFade side="top" />
      {children}
      {isPreview ? <ReasoningFade /> : null}
    </CollapsibleContent>
  );
}

function ReasoningText({
  className,
  children,
  ...props
}: React.ComponentProps<"div">) {
  const { preview: isPreview } = useContext(ReasoningPreviewContext);
  const scrollRef = useRef<HTMLDivElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!isPreview) return;
    const scrollEl = scrollRef.current;
    const contentEl = contentRef.current;
    if (!scrollEl || !contentEl) return;

    let pinned = true;
    let lastScrollTop = scrollEl.scrollTop;
    let lastScrollHeight = scrollEl.scrollHeight;
    const isAtBottom = () =>
      Math.abs(
        scrollEl.scrollHeight - scrollEl.scrollTop - scrollEl.clientHeight,
      ) <= 1 || scrollEl.scrollHeight <= scrollEl.clientHeight;

    const pin = () => {
      if (!pinned) return;
      scrollEl.scrollTop = scrollEl.scrollHeight;
    };
    // A pin's own scroll event can arrive after new content grew the scroll
    // height and read as "not at bottom"; only an upward move at unchanged
    // scroll height is user intent.
    const onScroll = () => {
      if (isAtBottom()) {
        pinned = true;
      } else if (
        scrollEl.scrollTop < lastScrollTop &&
        scrollEl.scrollHeight === lastScrollHeight
      ) {
        pinned = false;
      }
      lastScrollTop = scrollEl.scrollTop;
      lastScrollHeight = scrollEl.scrollHeight;
    };

    pin();
    scrollEl.addEventListener("scroll", onScroll);
    const observer = new ResizeObserver(pin);
    observer.observe(contentEl);
    return () => {
      scrollEl.removeEventListener("scroll", onScroll);
      observer.disconnect();
    };
  }, [isPreview]);

  return (
    <div
      ref={scrollRef}
      data-slot="reasoning-text"
      className={cn(
        "aui-reasoning-text chat-reasoning-text relative z-0 max-h-64 overflow-y-auto text-pretty",
        "transform-gpu transition-[transform,opacity] ease-[cubic-bezier(0.32,0.72,0,1)]",
        "motion-reduce:animate-none",
        "group-data-open/collapsible-content:animate-in",
        "group-data-closed/collapsible-content:animate-out",
        "group-data-open/collapsible-content:fade-in-0",
        "group-data-closed/collapsible-content:fade-out-0",
        "group-data-open/collapsible-content:slide-in-from-top-4",
        "group-data-closed/collapsible-content:slide-out-to-top-4",
        "group-data-open/collapsible-content:blur-in-[2px]",
        "group-data-closed/collapsible-content:blur-out-[2px]",
        "group-data-open/collapsible-content:animation-duration-(--animation-duration)",
        "group-data-closed/collapsible-content:animation-duration-(--animation-duration)",
        className,
      )}
      {...props}
    >
      <div ref={contentRef} className="aui-reasoning-text-content space-y-4">
        {children}
      </div>
    </div>
  );
}

export {
  ReasoningRoot,
  ReasoningTrigger,
  ReasoningContent,
  ReasoningText,
  ReasoningFade,
  reasoningVariants,
};
