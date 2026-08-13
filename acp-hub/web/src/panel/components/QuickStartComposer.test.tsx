import { fireEvent, render, screen, waitFor } from '@solidjs/testing-library';
import { createSignal } from 'solid-js';
import { afterEach, describe, expect, it } from 'vitest';
import { installPrincipalRole } from '../lib/auth-state';
import { QuickStartComposer } from './QuickStartComposer';

afterEach(() => installPrincipalRole(null));

describe('QuickStartComposer', () => {
  it('keeps the first message while changing durable project ownership', () => {
    installPrincipalRole('full');
    render(() => <QuickStartComposer projects={[{ id: 'alpha', name: 'Alpha' }, { id: 'beta', name: 'Beta' }]} />);

    const message = screen.getByRole('textbox', { name: '第一条消息' });
    const project = screen.getByRole('combobox', { name: '保存到项目' });
    fireEvent.input(message, { target: { value: '请检查这个项目' } });
    fireEvent.change(project, { target: { value: 'beta' } });

    expect(message).toHaveValue('请检查这个项目');
    expect(project).toHaveValue('beta');
    expect(message).toHaveAttribute('placeholder', '向 Beta 提问…');
  });

  it('falls back when the selected active project disappears before submit', async () => {
    installPrincipalRole('full');
    let setProjects!: (value: Array<{ id: string; name: string }>) => void;
    function Harness() {
      const [projects, update] = createSignal([{ id: 'alpha', name: 'Alpha' }, { id: 'beta', name: 'Beta' }]);
      setProjects = update;
      return <QuickStartComposer projects={projects()} />;
    }
    render(() => <Harness />);
    fireEvent.change(screen.getByRole('combobox', { name: '保存到项目' }), { target: { value: 'beta' } });
    setProjects([{ id: 'alpha', name: 'Alpha' }]);

    await waitFor(() => expect(screen.queryByRole('combobox', { name: '保存到项目' })).not.toBeInTheDocument());
    expect(screen.getByRole('textbox', { name: '第一条消息' })).toHaveAttribute('placeholder', '向 Alpha 提问…');
  });
});
