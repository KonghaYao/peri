export function authFeedback(status, phase = 'login') {
  if (status === 0) return { kind: 'network', message: '无法连接 acp-hub server。请确认本地服务仍在运行。', retryable: true };
  if (status === 401) {
    if (phase === 'status') return null;
    return { kind: 'credential', message: '令牌无效、已吊销，或不是可用于浏览器的 client token。', retryable: false };
  }
  if (status === 403) return { kind: 'origin', message: '当前页面来源未获 server 允许。请从 server 提供的本机地址打开面板。', retryable: false };
  if (status === 408) return { kind: 'network', message: '认证请求超时。请检查 server 状态后重试。', retryable: true };
  if (status === 413) return { kind: 'request', message: '令牌内容异常，已超过 server 接受的长度。', retryable: false };
  if (status === 429) return { kind: 'rate', message: '认证尝试过于频繁，请稍后再试。', retryable: true };
  if (status >= 500) return { kind: 'server', message: 'server 暂时无法完成认证；这不代表令牌有误。', retryable: true };
  return { kind: 'request', message: `认证请求失败（HTTP ${status}）。`, retryable: true };
}
