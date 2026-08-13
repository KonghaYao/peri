export function Spinner(props: { label?: string }) {
  return <span class="ui-spinner" role={props.label ? 'status' : undefined} aria-label={props.label} />;
}
