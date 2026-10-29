import type { ReactNode } from "react";
import { useFormName, useStateField, useTriggerAction } from "@openuidev/react-lang";
import { Button, Checkbox, Row, Select, Text, TextArea, TextInput, Toggle } from "../canvas";
import type { OpenUIRenderProps } from "./shared";
import { useOpenUIActionsEnabled } from "./shared";

type TriggerAction = ReturnType<typeof useTriggerAction>;
type TriggerActionArg = Parameters<TriggerAction>[2];

function FieldShell({ label, children }: { label?: string; children: ReactNode }) {
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 6, width: "100%" }}>
      {label ? (
        <Text size="small" tone="secondary">
          {label}
        </Text>
      ) : null}
      {children}
    </div>
  );
}

export interface TextInputShape {
  name: string;
  label?: string;
  placeholder?: string;
  type?: "text" | "email" | "url" | "search" | "number";
  defaultValue?: string;
}

export function TextInputView({ props }: OpenUIRenderProps<TextInputShape>) {
  const field = useStateField<string>(props.name, props.defaultValue);
  const enabled = useOpenUIActionsEnabled();
  return (
    <FieldShell label={props.label}>
      <TextInput
        value={field.value ?? ""}
        onChange={field.setValue}
        placeholder={props.placeholder}
        type={props.type}
        disabled={!enabled}
      />
    </FieldShell>
  );
}

export interface TextAreaShape {
  name: string;
  label?: string;
  placeholder?: string;
  rows?: number;
  defaultValue?: string;
}

export function TextAreaView({ props }: OpenUIRenderProps<TextAreaShape>) {
  const field = useStateField<string>(props.name, props.defaultValue);
  const enabled = useOpenUIActionsEnabled();
  return (
    <FieldShell label={props.label}>
      <TextArea
        value={field.value ?? ""}
        onChange={field.setValue}
        placeholder={props.placeholder}
        rows={props.rows}
        disabled={!enabled}
      />
    </FieldShell>
  );
}

export interface CheckboxShape {
  name: string;
  label: string;
  defaultValue?: boolean;
}

export function CheckboxView({ props }: OpenUIRenderProps<CheckboxShape>) {
  const field = useStateField<boolean>(props.name, props.defaultValue);
  const enabled = useOpenUIActionsEnabled();
  return (
    <Checkbox
      checked={field.value ?? false}
      onChange={field.setValue}
      label={props.label}
      disabled={!enabled}
    />
  );
}

export interface ToggleShape {
  name: string;
  label?: string;
  defaultValue?: boolean;
}

export function ToggleView({ props }: OpenUIRenderProps<ToggleShape>) {
  const field = useStateField<boolean>(props.name, props.defaultValue);
  const enabled = useOpenUIActionsEnabled();
  return (
    <Row justify="space-between" align="center">
      {props.label ? (
        <Text size="small" tone="secondary">
          {props.label}
        </Text>
      ) : null}
      <Toggle checked={field.value ?? false} onChange={field.setValue} disabled={!enabled} />
    </Row>
  );
}

export interface SelectShape {
  name: string;
  options: Array<{ value: string; label: string }>;
  label?: string;
  placeholder?: string;
  defaultValue?: string;
}

export function SelectView({ props }: OpenUIRenderProps<SelectShape>) {
  const field = useStateField<string>(props.name, props.defaultValue);
  const enabled = useOpenUIActionsEnabled();
  return (
    <FieldShell label={props.label}>
      <Select
        value={field.value ?? ""}
        onChange={field.setValue}
        options={props.options}
        placeholder={props.placeholder}
        disabled={!enabled}
      />
    </FieldShell>
  );
}

export interface ButtonShape {
  label: string;
  action?: TriggerActionArg;
  variant?: "primary" | "secondary" | "ghost";
  disabled?: boolean;
}

export function ButtonView({ props }: OpenUIRenderProps<ButtonShape>) {
  const trigger = useTriggerAction();
  const formName = useFormName();
  const enabled = useOpenUIActionsEnabled();
  return (
    <Button
      variant={props.variant}
      disabled={props.disabled || !enabled}
      onClick={() => {
        // schema 已要求 action；这里防御历史消息/异常解析，缺失时不发送 label。
        if (props.action === undefined) return;
        void trigger(props.label, formName, props.action);
      }}
    >
      {props.label}
    </Button>
  );
}
