"use client";

// 本地化定制（上游同步时逐项保留）：
// 1. 触发行文案中文化：按工具类别给出稳定摘要（如「读取文件 2、运行命令 1」），
//    没有类别信息时退回 `N 次工具调用`。
// 2. 触发行收敛为 24px 过程行；子调用缩进 22px + 8px 左边框。

import "@/styles/chat-tools.css";

import {
  memo,
  useCallback,
  useRef,
  useState,
  type FC,
  type PropsWithChildren,
} from "react";
import { ChevronDownIcon, LoaderIcon } from "lucide-react";
import { cva, type VariantProps } from "class-variance-authority";
import { useScrollLock } from "@assistant-ui/react";
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

const ANIMATION_DURATION = 200;

/**
 * 实际工具名：`ExecuteExtraTool` 是 deferred MCP 工具的元调用，界面按它参数里的
 * `tool_name` 展示真实操作（如 `mcp__local-fs__Write`）。解析失败时保留原名。
 */
export function toolDisplayName(toolName: string, args: unknown): string {
  if (toolName !== "ExecuteExtraTool" || typeof args !== "object" || args === null) {
    return toolName;
  }
  const inner = (args as Record<string, unknown>).tool_name;
  return typeof inner === "string" && inner.trim() !== "" ? inner : toolName;
}

/** 工具名 → 过程标题里的稳定类别。camelCase、snake_case 与 MCP 前缀都先拆词。 */
function toolCategory(toolName: string): string {
  const tokens = toolName
    .replace(/([a-z0-9])([A-Z])/g, "$1 $2")
    .toLowerCase()
    .split(/[^a-z0-9]+/)
    .filter(Boolean);
  const has = (...values: string[]) => values.some((value) => tokens.includes(value));

  if (has("read") && has("image")) return "读取图片";
  if (has("web") && has("search")) return "搜索网页";
  if (has("web") && has("fetch", "browse")) return "访问网页";
  if (has("todo", "plan", "goal")) return "更新计划";
  if (has("question", "questions") || (has("ask") && has("user"))) return "提问";
  if (has("subagent", "subagents", "agent", "task")) return "子智能体";
  if (has("read", "readfile")) return "读取文件";
  if (has("grep", "glob", "search")) return "搜索代码";
  if (has("write") && has("stdin")) return "运行命令";
  if (has("write", "writefile")) return "写入文件";
  if (has("edit", "patch", "multiedit")) return "编辑文件";
  if (has("run") && has("code")) return "运行代码";
  if (has("bash", "shell", "exec", "command", "commands", "terminal", "stdin")) return "运行命令";
  return "工具";
}

/**
 * 过程组标题：按类别计数，降序排列，并列按首次出现。
 * 不再用「等」截断类别——触发行会省略超长文本，但 `title` 保留全文；
 * 只依赖已出现的工具名，因此流式追加内容时标题不会因运行态切换而闪烁。
 */
export function summarizeToolGroup(toolNames: readonly string[]): string {
  if (toolNames.length === 0) return "";
  const counts = new Map<string, number>();
  for (const toolName of toolNames) {
    const category = toolCategory(toolName);
    counts.set(category, (counts.get(category) ?? 0) + 1);
  }
  const ranked = [...counts.entries()].sort((left, right) => right[1] - left[1]);
  return ranked.map(([category, count]) => `${category} ${count}`).join("、");
}

const toolGroupVariants = cva("aui-tool-group-root group/tool-group w-full", {
  variants: {
    variant: {
      outline: "rounded-lg border py-3",
      ghost: "",
      muted: "border-muted-foreground/30 bg-muted/30 rounded-lg border py-3",
    },
  },
  defaultVariants: { variant: "outline" },
});

export type ToolGroupRootProps = Omit<
  React.ComponentProps<typeof Collapsible>,
  "open" | "onOpenChange"
> &
  VariantProps<typeof toolGroupVariants> & {
    open?: boolean;
    onOpenChange?: (open: boolean) => void;
    defaultOpen?: boolean;
  };

function ToolGroupRoot({
  className,
  variant,
  open: controlledOpen,
  onOpenChange: controlledOnOpenChange,
  defaultOpen = false,
  children,
  ...props
}: ToolGroupRootProps) {
  const collapsibleRef = useRef<HTMLDivElement>(null);
  const [uncontrolledOpen, setUncontrolledOpen] = useState(defaultOpen);
  const lockScroll = useScrollLock(collapsibleRef, ANIMATION_DURATION);

  const isControlled = controlledOpen !== undefined;
  const isOpen = isControlled ? controlledOpen : uncontrolledOpen;

  const handleOpenChange = useCallback(
    (open: boolean) => {
      lockScroll();
      if (!isControlled) {
        setUncontrolledOpen(open);
      }
      controlledOnOpenChange?.(open);
    },
    [lockScroll, isControlled, controlledOnOpenChange],
  );

  return (
    <Collapsible
      ref={collapsibleRef}
      data-slot="tool-group-root"
      data-variant={variant ?? "outline"}
      open={isOpen}
      onOpenChange={handleOpenChange}
      className={cn(
        toolGroupVariants({ variant }),
        "group/tool-group-root",
        className,
      )}
      style={
        {
          "--animation-duration": `${ANIMATION_DURATION}ms`,
        } as React.CSSProperties
      }
      {...props}
    >
      {children}
    </Collapsible>
  );
}

function ToolGroupTrigger({
  count,
  summary,
  active = false,
  className,
  ...props
}: React.ComponentProps<typeof CollapsibleTrigger> & {
  count: number;
  /** 类别摘要；缺省时退回 `N 次工具调用`。 */
  summary?: string;
  active?: boolean;
}) {
  const label = summary && summary !== "" ? summary : `${count} 次工具调用`;

  return (
    <CollapsibleTrigger
      data-slot="tool-group-trigger"
      data-expandable="true"
      title={label}
      className={cn(
        "aui-tool-group-trigger chat-tool-group-trigger group/trigger origin-left",
        processRow,
        "group-data-[variant=ghost]/tool-group-root:text-muted-foreground group-data-[variant=ghost]/tool-group-root:hover:text-foreground group-data-[variant=ghost]/tool-group-root:py-1.5",
        "group-data-[variant=outline]/tool-group-root:w-full group-data-[variant=outline]/tool-group-root:px-4",
        "group-data-[variant=muted]/tool-group-root:w-full group-data-[variant=muted]/tool-group-root:px-4",
        className,
      )}
      {...props}
    >
      <span
        aria-hidden="true"
        className={cn(processLeading, "chat-tool-group-leading")}
      >
        {active && (
          <LoaderIcon
            data-slot="tool-group-trigger-loader"
            className="aui-tool-group-trigger-loader shrink-0 animate-spin [animation-duration:0.6s] motion-reduce:animate-none"
          />
        )}
        <ChevronDownIcon
          data-slot="tool-group-trigger-chevron"
          className={cn(processChevron, "chat-tool-group-chevron")}
        />
      </span>
      <span
        data-slot="tool-group-trigger-label"
        className={cn(
          "aui-tool-group-trigger-label-wrapper chat-tool-group-label inline-block text-start",
          processTitle,
          "group-data-[variant=ghost]/tool-group-root:font-normal",
          "group-data-[variant=outline]/tool-group-root:grow",
          "group-data-[variant=muted]/tool-group-root:grow",
          active && "shimmer motion-reduce:animate-none",
        )}
      >
        {label}
      </span>
    </CollapsibleTrigger>
  );
}

function ToolGroupContent({
  className,
  children,
  ...props
}: React.ComponentProps<typeof CollapsibleContent>) {
  return (
    <CollapsibleContent
      data-slot="tool-group-content"
      className={cn(
        "aui-tool-group-content relative overflow-hidden text-sm outline-none",
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
      <div
        className={cn(
          "chat-tool-group-inner",
          "mt-2 flex flex-col gap-2",
          "group-data-[variant=ghost]/tool-group-root:mt-1 group-data-[variant=ghost]/tool-group-root:gap-1",
          "group-data-[variant=outline]/tool-group-root:mt-3 group-data-[variant=outline]/tool-group-root:border-t group-data-[variant=outline]/tool-group-root:px-4 group-data-[variant=outline]/tool-group-root:pt-3",
          "group-data-[variant=muted]/tool-group-root:mt-3 group-data-[variant=muted]/tool-group-root:border-t group-data-[variant=muted]/tool-group-root:px-4 group-data-[variant=muted]/tool-group-root:pt-3",
          "[&>*]:animate-in [&>*]:fade-in-0 [&>*]:blur-in-[2px] [&>*]:slide-in-from-top-1 [&>*]:animation-duration-(--animation-duration) [&>*]:ease-[cubic-bezier(0.32,0.72,0,1)]",
          "[&>*]:motion-reduce:animate-none",
          "[&>*:nth-child(2)]:[animation-delay:40ms]",
          "[&>*:nth-child(3)]:[animation-delay:80ms]",
          "[&>*:nth-child(4)]:[animation-delay:120ms]",
          "[&>*:nth-child(n+5)]:[animation-delay:160ms]",
        )}
      >
        {children}
      </div>
    </CollapsibleContent>
  );
}

type ToolGroupComponent = FC<
  PropsWithChildren<{ startIndex: number; endIndex: number }>
> & {
  Root: typeof ToolGroupRoot;
  Trigger: typeof ToolGroupTrigger;
  Content: typeof ToolGroupContent;
};

const ToolGroupImpl: FC<
  PropsWithChildren<{ startIndex: number; endIndex: number }>
> = ({ children, startIndex, endIndex }) => {
  const toolCount = endIndex - startIndex + 1;

  return (
    <ToolGroupRoot>
      <ToolGroupTrigger count={toolCount} />
      <ToolGroupContent>{children}</ToolGroupContent>
    </ToolGroupRoot>
  );
};

/**
 * @deprecated This wrapper targets the legacy `components.ToolGroup` prop
 * on `<MessagePrimitive.Parts>`. Use `<MessagePrimitive.GroupedParts>` with
 * a `groupBy` returning `"group-tool"` and compose `ToolGroupRoot` /
 * `ToolGroupTrigger` / `ToolGroupContent` directly. See `thread.tsx`.
 */
const ToolGroup = memo(ToolGroupImpl) as unknown as ToolGroupComponent;

ToolGroup.displayName = "ToolGroup";
ToolGroup.Root = ToolGroupRoot;
ToolGroup.Trigger = ToolGroupTrigger;
ToolGroup.Content = ToolGroupContent;

export {
  ToolGroup,
  ToolGroupRoot,
  ToolGroupTrigger,
  ToolGroupContent,
  toolGroupVariants,
};
