// acp-hub 验证台 —— WebSocket 探测。
// 连接指定端点并如实记录 open/close/error 事件：握手是否成功、服务器以何
// 种方式关闭（无 token 时 hub 会在首帧超时 10s 后以 1011 关闭——本页只做
// 观察者，不做认证，日志如实展示，不造假）。
(function () {
  'use strict';

  const logEl = document.getElementById('ws-log');
  let socket = null;

  function log(line) {
    if (!logEl) return;
    const time = new Date().toLocaleTimeString();
    logEl.textContent += '[' + time + '] ' + line + '\n';
    logEl.scrollTop = logEl.scrollHeight;
  }

  function connect() {
    const endpoint = document.getElementById('ws-endpoint').value.trim();
    if (socket) {
      socket.close();
      socket = null;
    }
    log('连接 ' + endpoint + ' …');
    let ws;
    try {
      ws = new WebSocket(endpoint);
    } catch (e) {
      log('构造 WebSocket 失败: ' + e.message);
      return;
    }
    socket = ws;
    ws.onopen = function () {
      log('握手成功（open）—— hub 接受任意路径的 ws 升级请求');
    };
    ws.onmessage = function (ev) {
      log('收到消息: ' + ev.data);
    };
    ws.onerror = function () {
      log('error 事件（详见浏览器控制台）');
    };
    ws.onclose = function (ev) {
      log(
        '已关闭: code=' + ev.code + ' reason=' + (ev.reason || '（空）') +
        (ev.wasClean ? '（正常关闭）' : '')
      );
      socket = null;
    };
  }

  // 统一入口：htmx（hx-on）与 app.js 兜底都调用它。
  window.acpHubWsConnect = connect;
})();
