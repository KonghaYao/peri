import { render, screen } from '@solidjs/testing-library';
import { afterEach, describe, expect, it } from 'vitest';
import { reportTransportIssue, retainPersistentErrors, setPersistentErrors, type PersistentError } from '../store';
import { ErrorCenter } from './ErrorCenter';

afterEach(() => setPersistentErrors([]));

it('never evicts unknown-result recovery cards in favor of ordinary errors', () => {
  const uncertain: PersistentError = { id: 1, title: '结果尚未确认', detail: '需要使用原请求确认', commandId: 'cmd-1', retryable: true, retrying: false };
  let errors: PersistentError[] = [uncertain];
  for (let id = 2; id <= 9; id += 1) {
    errors = retainPersistentErrors(errors, { id, title: `error ${id}`, detail: 'ordinary', commandId: null, retryable: false, retrying: false });
  }
  expect(errors).toHaveLength(5);
  expect(errors).toContainEqual(uncertain);
  expect(errors.slice(1).map((error) => error.id)).toEqual([6, 7, 8, 9]);
});

it('keeps every recovery card when more than five operations became uncertain together', () => {
  let errors: PersistentError[] = [];
  for (let id = 1; id <= 6; id += 1) {
    errors = retainPersistentErrors(errors, { id, title: `uncertain ${id}`, detail: 'blocked', commandId: `cmd-${id}`, retryable: true, retrying: false });
  }
  expect(errors.map((error) => error.id)).toEqual([1, 2, 3, 4, 5, 6]);
});

describe('ErrorCenter', () => {
  it('offers same-command confirmation only for registered uncertain actions', () => {
    setPersistentErrors([
      { id: 1, title: '结果尚未确认', detail: '等待同步', commandId: 'safe-command', retryable: true, retrying: false },
      { id: 2, title: '无权限', detail: '需要 full 权限', commandId: 'forbidden', retryable: false, retrying: false },
    ]);
    render(() => <ErrorCenter />);

    expect(screen.getAllByRole('alert')).toHaveLength(2);
    expect(screen.getByRole('button', { name: '使用原请求重新确认' })).toBeInTheDocument();
    expect(screen.getByText('结果尚未确认').closest('.error-card')).not.toHaveTextContent('关闭');
    expect(screen.getByText('无权限').closest('.error-card')).not.toHaveTextContent('使用原请求重新确认');
  });

  it('surfaces one payload-free transport problem instead of transient toasts', () => {
    reportTransportIssue({ kind: 'malformed_frame', size: 17 });
    reportTransportIssue({ kind: 'malformed_frame', size: 29 });
    render(() => <ErrorCenter />);

    expect(screen.getAllByRole('alert')).toHaveLength(1);
    expect(screen.getByText('收到格式错误的服务器数据')).toBeInTheDocument();
    expect(screen.getByText(/29 个字符/)).toBeInTheDocument();
  });
});
