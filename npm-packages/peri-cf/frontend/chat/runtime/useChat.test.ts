import { describe, expect, test } from 'bun:test';
import { errorBanners, syncRetryAction } from './useChat';

describe('chat error banner routing', () => {
  test('list errors and chat errors are shown separately with matching actions', () => {
    const banners = errorBanners({ listError: '列表失败', chatError: '会话失败', loadState: 'ready' });
    expect(banners.list).toMatchObject({ message: '列表失败', action: 'refresh-list', dismissible: true });
    // 执行错误不是读面失败：不提供 reload，避免重读同一条错误。
    expect(banners.chat).toMatchObject({ message: '会话失败', action: 'none', dismissible: true });
  });

  test('a list error never steals the retry action of a failed chat read', () => {
    const banners = errorBanners({ listError: '列表失败', chatError: '读取失败', loadState: 'failed' });
    expect(banners.list?.action).toBe('refresh-list');
    expect(banners.chat).toMatchObject({ action: 'reload-chat', dismissible: false });
  });

  test('a failed read keeps a retry entry after the error text is dismissed', () => {
    expect(errorBanners({ listError: '', chatError: '', loadState: 'failed' }).chat)
      .toMatchObject({ message: '会话加载失败，请重新加载。', action: 'reload-chat', dismissible: false });
  });

  test('a new-chat creation failure is a read failure and can be retried', () => {
    expect(errorBanners({ listError: '', chatError: '创建失败', loadState: 'failed' }).chat)
      .toMatchObject({ message: '创建失败', action: 'reload-chat' });
  });

  test('no errors means no banners', () => {
    expect(errorBanners({ listError: '', chatError: '', loadState: 'ready' })).toEqual({});
  });

  test('unknown results and stale bindings keep a non-dismissible reload entry', () => {
    for (const chatError of ['网络请求失败，执行状态尚未确认。', '会话绑定已过期，请重新同步后再发送。']) {
      const banner = errorBanners({ listError: '', chatError, loadState: 'ready', recovery: 'reload' }).chat;
      expect(banner).toMatchObject({ message: chatError, action: 'reload-chat', dismissible: false });
    }
  });
});

describe('live sync retry routing', () => {
  test('a mid-stream disconnect keeps a reconnect entry even with content', () => {
    expect(syncRetryAction({ loadState: 'ready', liveSync: 'failed' })).toBe('reload-chat');
  });

  test('a first-frame failure is retryable while syncing and idle are not', () => {
    expect(syncRetryAction({ loadState: 'failed', liveSync: 'failed' })).toBe('reload-chat');
    expect(syncRetryAction({ loadState: 'loading', liveSync: 'connecting' })).toBe('none');
    expect(syncRetryAction({ loadState: 'ready', liveSync: 'synced' })).toBe('none');
  });
});
