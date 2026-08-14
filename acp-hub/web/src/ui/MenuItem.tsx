import { splitProps, type JSX } from 'solid-js';

type Props = JSX.ButtonHTMLAttributes<HTMLButtonElement> & {
  tone?: 'default' | 'danger';
};

export function MenuItem(props: Props) {
  const [local, button] = splitProps(props, ['tone', 'class', 'children']);
  return <button
    {...button}
    type={button.type ?? 'button'}
    role="menuitem"
    class={`ui-menu__item ${local.tone === 'danger' ? 'ui-menu__item--danger' : ''} ${local.class ?? ''}`}
  >{local.children}</button>;
}
