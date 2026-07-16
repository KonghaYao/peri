// ========== Solid 组件包装 ==========

import html from 'solid-js/html';
import { useParentState } from '/lib/solid-hooks.js';

/**
 * 声明式订阅 shared state。等价于 useParentState 但更声明式。
 * 用法：
 *   <SharedState key="currentFile">${(file, setFile) => html`...`}<//>
 *
 * @param {{ key: string, children: (value: () => any, setter: (v: any) => Promise<void>) => any }} props
 */
export function SharedState(props) {
  const [value, setter] = useParentState(props.key);
  return props.children(value, setter);
}
