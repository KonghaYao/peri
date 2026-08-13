import { Show, type JSX } from 'solid-js';
export function Menu(props: { open: boolean; children: JSX.Element }) { return <Show when={props.open}><div class="ui-menu">{props.children}</div></Show>; }
