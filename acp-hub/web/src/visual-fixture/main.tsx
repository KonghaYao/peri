import { onCleanup } from 'solid-js';
import { render } from 'solid-js/web';
import '../styles.css';
import './fixture.css';
import { AppShell } from '../panel/components/AppShell';
import { Toasts } from '../panel/components/Toasts';
import { DEFAULT_VISUAL_SCENARIO, installVisualScenario, visualScenarios } from './scenarios';

const selected = new URLSearchParams(window.location.search).get('scenario') || DEFAULT_VISUAL_SCENARIO;

function VisualFixture() {
  const installed = installVisualScenario(selected);
  onCleanup(installed.dispose);
  return <>
    <div class="authenticated-app visual-fixture-root">
      <aside class="visual-fixture-rail" aria-label="视觉验收场景">
        <strong>UI 状态验收台</strong>
        <span>静态测试数据 · 操作不会连接 server</span>
        <nav>{visualScenarios.map((scenario) => <a aria-current={scenario.id === installed.scenario.id ? 'page' : undefined} href={`?scenario=${scenario.id}`} title={scenario.description}>{scenario.label}</a>)}</nav>
        <code>{installed.scenario.controls}</code>
      </aside>
      <div class="visual-fixture-stage"><AppShell /></div>
    </div>
    <Toasts />
  </>;
}

render(() => <VisualFixture />, document.getElementById('app')!);
