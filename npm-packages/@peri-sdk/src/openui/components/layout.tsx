import {
  Code,
  Divider,
  Grid,
  H1,
  H2,
  H3,
  Link,
  Pill,
  Row,
  Spacer,
  Stack,
  Text,
} from "../canvas";
import type { OpenUIRenderProps } from "./shared";
import { safeHref } from "./shared";

export interface StackShape {
  children?: unknown;
  gap?: number;
}

export function StackView({ props, renderNode }: OpenUIRenderProps<StackShape>) {
  return <Stack gap={props.gap}>{renderNode(props.children)}</Stack>;
}

export interface RowShape {
  children?: unknown;
  gap?: number;
  align?: "start" | "center" | "end" | "stretch";
  justify?: "start" | "center" | "end" | "space-between";
  wrap?: boolean;
}

export function RowView({ props, renderNode }: OpenUIRenderProps<RowShape>) {
  return (
    <Row gap={props.gap} align={props.align} justify={props.justify} wrap={props.wrap}>
      {renderNode(props.children)}
    </Row>
  );
}

export interface GridShape {
  columns: number;
  children?: unknown;
  gap?: number;
  align?: "start" | "center" | "end" | "stretch";
}

export function GridView({ props, renderNode }: OpenUIRenderProps<GridShape>) {
  return (
    <Grid columns={props.columns} gap={props.gap} align={props.align}>
      {renderNode(props.children)}
    </Grid>
  );
}

export function SpacerView() {
  return <Spacer />;
}

export function DividerView() {
  return <Divider />;
}

export interface TextShape {
  content: string | number;
  tone?: "primary" | "secondary" | "tertiary" | "quaternary";
  size?: "body" | "small";
  weight?: "normal" | "medium" | "semibold" | "bold";
  italic?: boolean;
  truncate?: boolean;
}

export function TextView({ props }: OpenUIRenderProps<TextShape>) {
  return (
    <Text
      tone={props.tone}
      size={props.size}
      weight={props.weight}
      italic={props.italic}
      truncate={props.truncate}
    >
      {props.content}
    </Text>
  );
}

export interface HeadingShape {
  content: string;
}

export function H1View({ props }: OpenUIRenderProps<HeadingShape>) {
  return <H1>{props.content}</H1>;
}

export function H2View({ props }: OpenUIRenderProps<HeadingShape>) {
  return <H2>{props.content}</H2>;
}

export function H3View({ props }: OpenUIRenderProps<HeadingShape>) {
  return <H3>{props.content}</H3>;
}

export interface CodeShape {
  content: string;
}

export function CodeView({ props }: OpenUIRenderProps<CodeShape>) {
  return <Code>{props.content}</Code>;
}

export interface LinkShape {
  label: string;
  href: string;
}

export function LinkView({ props }: OpenUIRenderProps<LinkShape>) {
  const href = safeHref(props.href);
  if (href === undefined) {
    // 不渲染不可点击的伪链接；把原因放在 title 里，便于排查模型输出。
    return <span title={`Blocked non-http(s) link: ${props.href}`}>{props.label}</span>;
  }
  return <Link href={href}>{props.label}</Link>;
}

export interface PillShape {
  label: string;
  size?: "sm" | "md";
}

export function PillView({ props }: OpenUIRenderProps<PillShape>) {
  return <Pill size={props.size}>{props.label}</Pill>;
}
