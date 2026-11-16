import { afterEach, describe, expect, mock, test } from 'bun:test';
import { installDom } from './dom';

installDom();

const { cleanup, fireEvent, render, screen } = await import('@testing-library/react');
const { McpServerForm } = await import('../../src/features/mcp/mcp-server-form');

afterEach(() => {
  cleanup();
});

function renderForm(existingNames: readonly string[]) {
  const onSave = mock(() => {});
  render(<McpServerForm existingNames={existingNames} onSave={onSave} onCancel={() => {}} />);
  return { onSave };
}

describe('McpServerForm', () => {
  test('rejects duplicate names and invalid URLs before saving', () => {
    const { onSave } = renderForm(['docs']);
    const name = screen.getByLabelText('服务器名称');
    const url = screen.getByLabelText('URL');

    fireEvent.change(name, { target: { value: 'docs' } });
    expect(screen.getByText('名称已被使用')).toBeTruthy();

    fireEvent.change(name, { target: { value: 'fresh' } });
    expect(screen.queryByText('名称已被使用')).toBeNull();

    fireEvent.change(url, { target: { value: 'not-a-url' } });
    expect(screen.getByText('请输入合法的 http(s) URL')).toBeTruthy();

    fireEvent.click(screen.getByRole('button', { name: '添加服务器' }));
    expect(onSave).not.toHaveBeenCalled();

    fireEvent.change(url, { target: { value: 'https://example.com/mcp' } });
    expect(screen.queryByText('请输入合法的 http(s) URL')).toBeNull();

    fireEvent.click(screen.getByRole('button', { name: '添加服务器' }));
    expect(onSave).toHaveBeenCalledWith('fresh', {
      type: 'http',
      url: 'https://example.com/mcp',
    });
  });

  test('requires a command for stdio servers and normalises args', () => {
    const { onSave } = renderForm([]);

    fireEvent.change(screen.getByLabelText('服务器名称'), { target: { value: 'local' } });
    fireEvent.click(screen.getByRole('button', { name: 'stdio' }));
    fireEvent.click(screen.getByRole('button', { name: '添加服务器' }));

    expect(screen.getByText('请输入启动命令')).toBeTruthy();
    expect(onSave).not.toHaveBeenCalled();

    fireEvent.change(screen.getByLabelText('启动命令'), { target: { value: 'npx' } });
    fireEvent.change(screen.getByLabelText('参数（每行一个）'), {
      target: { value: '  -y \n server-everything \n' },
    });
    fireEvent.click(screen.getByRole('button', { name: '添加服务器' }));

    expect(onSave).toHaveBeenCalledWith('local', {
      type: 'stdio',
      command: 'npx',
      args: ['-y', 'server-everything'],
    });
  });
});
