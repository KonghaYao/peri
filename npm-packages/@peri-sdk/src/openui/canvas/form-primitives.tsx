import {
  useCallback,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type JSX,
  type ReactNode,
} from "react";
import { useHostTheme } from "./hooks";
import { mergeStyle } from "./merge-style";
import { canvasRadius, canvasSpacing } from "./theme";

export type TextInputProps = {
  value?: string;
  onChange?: (value: string) => void;
  placeholder?: string;
  disabled?: boolean;
  type?: "text" | "email" | "password" | "number" | "url" | "search";
  style?: CSSProperties;
};

export function TextInput({
  value,
  onChange,
  placeholder,
  disabled = false,
  type = "text",
  style,
}: TextInputProps): JSX.Element {
  const { tokens } = useHostTheme();
  return (
    <input
      type={type}
      value={value}
      onChange={onChange ? (event) => onChange(event.target.value) : undefined}
      placeholder={placeholder}
      disabled={disabled}
      style={mergeStyle(
        {
          boxSizing: "border-box",
          width: "100%",
          height: "28px",
          padding: `${canvasSpacing[1]}px ${canvasSpacing[2]}px`,
          border: `1px solid ${tokens.stroke.secondary}`,
          borderRadius: `${canvasRadius.md}px`,
          background: tokens.fill.tertiary,
          color: tokens.text.primary,
          fontSize: "13px",
          lineHeight: "18px",
          letterSpacing: "0.13px",
          fontFamily: "inherit",
          outline: "none",
          opacity: disabled ? 0.5 : 1,
          cursor: disabled ? "not-allowed" : "text",
        },
        style,
      )}
    />
  );
}

export type TextAreaProps = {
  value?: string;
  onChange?: (value: string) => void;
  placeholder?: string;
  disabled?: boolean;
  rows?: number;
  style?: CSSProperties;
};

export function TextArea({
  value,
  onChange,
  placeholder,
  disabled = false,
  rows = 3,
  style,
}: TextAreaProps): JSX.Element {
  const { tokens } = useHostTheme();
  const ref = useRef<HTMLTextAreaElement>(null);
  const resize = useCallback(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  }, []);
  useLayoutEffect(() => {
    resize();
  }, [resize, value]);
  return (
    <textarea
      ref={ref}
      value={value}
      onChange={onChange ? (event) => onChange(event.target.value) : undefined}
      onInput={resize}
      placeholder={placeholder}
      disabled={disabled}
      rows={rows}
      style={mergeStyle(
        {
          boxSizing: "border-box",
          width: "100%",
          minHeight: "28px",
          padding: `${canvasSpacing[1]}px ${canvasSpacing[2]}px`,
          border: `1px solid ${tokens.stroke.secondary}`,
          borderRadius: `${canvasRadius.md}px`,
          background: tokens.fill.tertiary,
          color: tokens.text.primary,
          fontSize: "13px",
          lineHeight: "18px",
          letterSpacing: "0.13px",
          fontFamily: "inherit",
          outline: "none",
          resize: "none",
          overflow: "hidden",
          opacity: disabled ? 0.5 : 1,
          cursor: disabled ? "not-allowed" : "text",
        },
        style,
      )}
    />
  );
}

export type CheckboxProps = {
  checked?: boolean;
  onChange?: (checked: boolean) => void;
  disabled?: boolean;
  label?: ReactNode;
  style?: CSSProperties;
};

export function Checkbox({
  checked = false,
  onChange,
  disabled = false,
  label,
  style,
}: CheckboxProps): JSX.Element {
  const { tokens } = useHostTheme();
  const box: CSSProperties = {
    boxSizing: "border-box",
    display: "inline-flex",
    alignItems: "center",
    justifyContent: "center",
    width: "14px",
    height: "14px",
    borderRadius: `${canvasRadius.sm}px`,
    border: `1px solid ${checked ? tokens.accent.control : tokens.stroke.primary}`,
    background: checked ? tokens.accent.control : tokens.fill.tertiary,
    flexShrink: 0,
    cursor: disabled ? "not-allowed" : "pointer",
    opacity: disabled ? 0.5 : 1,
    transition: "background 120ms ease, border-color 120ms ease",
  };
  return (
    <label
      style={mergeStyle(
        {
          position: "relative",
          display: "inline-flex",
          alignItems: "center",
          gap: `${canvasSpacing[2]}px`,
          cursor: disabled ? "not-allowed" : "pointer",
          userSelect: "none",
          fontSize: "13px",
          lineHeight: "18px",
          color: tokens.text.primary,
        },
        style,
      )}
    >
      <input
        type="checkbox"
        checked={checked}
        disabled={disabled}
        onChange={(event) => onChange?.(event.target.checked)}
        style={{
          position: "absolute",
          width: "1px",
          height: "1px",
          margin: 0,
          opacity: 0,
          pointerEvents: "none",
        }}
      />
      <span style={box}>
        {checked ? (
          <svg
            width={10}
            height={10}
            viewBox="0 0 10 10"
            fill="none"
            xmlns="http://www.w3.org/2000/svg"
            aria-hidden="true"
            style={{ display: "block" }}
          >
            <path
              d="M2 5.2 4.2 7.5 8 3"
              stroke={tokens.text.onAccent}
              strokeWidth={1.5}
              strokeLinecap="round"
              strokeLinejoin="round"
            />
          </svg>
        ) : null}
      </span>
      {label != null ? <span>{label}</span> : null}
    </label>
  );
}

export type ToggleProps = {
  checked?: boolean;
  onChange?: (checked: boolean) => void;
  disabled?: boolean;
  size?: "sm" | "md";
  style?: CSSProperties;
};

const toggleSizes = {
  sm: { track: 16, width: 26, knobOffset: 2 },
  md: { track: 20, width: 32, knobOffset: 3 },
};

export function Toggle({
  checked = false,
  onChange,
  disabled = false,
  size = "sm",
  style,
}: ToggleProps): JSX.Element {
  const { tokens } = useHostTheme();
  const dim = toggleSizes[size];
  const knob = dim.track - 2 * dim.knobOffset;
  const travel = dim.width - dim.track;
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      disabled={disabled}
      onClick={disabled ? undefined : () => onChange?.(!checked)}
      style={mergeStyle(
        {
          boxSizing: "border-box",
          position: "relative",
          display: "inline-flex",
          alignItems: "center",
          width: `${dim.width}px`,
          height: `${dim.track}px`,
          borderRadius: `${canvasRadius.full}px`,
          background: checked ? tokens.accent.control : tokens.fill.secondary,
          border: "none",
          padding: 0,
          cursor: disabled ? "not-allowed" : "pointer",
          opacity: disabled ? 0.5 : 1,
          transition: "background 150ms ease",
          flexShrink: 0,
          fontFamily: "inherit",
        },
        style,
      )}
    >
      <span
        aria-hidden="true"
        style={{
          position: "absolute",
          width: `${knob}px`,
          height: `${knob}px`,
          borderRadius: `${canvasRadius.full}px`,
          background: "white",
          left: `${dim.knobOffset}px`,
          transform: checked ? `translateX(${travel}px)` : "translateX(0)",
          transition: "transform 150ms ease",
        }}
      />
    </button>
  );
}

export type SelectOption = {
  value: string;
  label: string;
  disabled?: boolean;
};

export type SelectProps = {
  value?: string;
  onChange?: (value: string) => void;
  options: SelectOption[];
  placeholder?: string;
  disabled?: boolean;
  style?: CSSProperties;
};

export function Select({
  value,
  onChange,
  options = [],
  placeholder,
  disabled = false,
  style,
}: SelectProps): JSX.Element {
  const { tokens, kind } = useHostTheme();
  const hasValue = value !== undefined && value !== "";
  return (
    <span
      style={mergeStyle(
        { position: "relative", display: "inline-flex", alignItems: "center" },
        style,
      )}
    >
      <select
        value={value ?? ""}
        onChange={onChange ? (event) => onChange(event.target.value) : undefined}
        disabled={disabled}
        style={{
          boxSizing: "border-box",
          appearance: "none",
          WebkitAppearance: "none",
          background: "transparent",
          colorScheme: kind === "light" || kind === "hc-light" ? "light" : "dark",
          border: `1px solid ${tokens.stroke.tertiary}`,
          borderRadius: `${canvasRadius.md}px`,
          color: hasValue ? tokens.text.primary : tokens.text.tertiary,
          fontSize: "13px",
          lineHeight: "18px",
          fontFamily: "inherit",
          padding: `4px ${canvasSpacing[5]}px 4px 4px`,
          outline: "none",
          cursor: disabled ? "not-allowed" : "pointer",
          opacity: disabled ? 0.5 : 1,
          minWidth: 0,
        }}
      >
        {placeholder != null ? (
          <option
            value=""
            disabled
            style={{ background: tokens.bg.elevated, color: tokens.text.tertiary }}
          >
            {placeholder}
          </option>
        ) : null}
        {options.map((option) => (
          <option
            key={option.value}
            value={option.value}
            disabled={option.disabled}
            style={{ background: tokens.bg.elevated, color: tokens.text.primary }}
          >
            {option.label}
          </option>
        ))}
      </select>
      <span
        aria-hidden="true"
        style={{
          position: "absolute",
          right: "4px",
          top: "50%",
          transform: "translateY(-50%)",
          pointerEvents: "none",
          color: tokens.text.secondary,
          display: "flex",
          alignItems: "center",
        }}
      >
        <svg
          width={10}
          height={10}
          viewBox="0 0 12 12"
          fill="none"
          xmlns="http://www.w3.org/2000/svg"
          aria-hidden="true"
        >
          <path
            d="M3 4.5 6 7.5 9 4.5"
            stroke="currentColor"
            strokeWidth={1.2}
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        </svg>
      </span>
    </span>
  );
}
