// acp-hub 验证台 —— 页面初始化。
// 填充服务器信息与端点清单；绑定「连接」按钮（htmx 的 hx-on 已接管时跳过，
// CDN 离线时 addEventListener 兜底，保证页面始终可用）。
(function () {
  'use strict';

  // 服务器信息取自当前页面 URL（页面只能经 http:// 访问本服务）。
  const host = window.location.hostname || '127.0.0.1';
  const port = window.location.port || '8456';

  document.getElementById('info-host').textContent = host + ':' + port;
  document.getElementById('ws-endpoint').value = 'ws://' + host + ':' + port + '/';

  // 端点清单（server 不校验 ws 路径，路径仅为文档约定）。
  const endpoints = [
    { label: 'client（TUI）时序入口', url: 'ws://' + host + ':' + port + '/' },
    { label: 'instance 连接入口（约定）', url: 'ws://' + host + ':' + port + '/instance' },
  ];
  const list = document.getElementById('endpoint-list');
  for (const item of endpoints) {
    const li = document.createElement('li');
    const code = document.createElement('code');
    code.textContent = item.url;
    li.textContent = item.label + ' → ';
    li.appendChild(code);
    list.appendChild(li);
  }

  // htmx 未加载（CDN 失败/离线）时用原生事件兜底；二者只取其一，避免重复触发。
  const btn = document.getElementById('ws-connect');
  if (typeof window.htmx === 'undefined') {
    btn.addEventListener('click', function () {
      window.acpHubWsConnect();
    });
  }
})();
