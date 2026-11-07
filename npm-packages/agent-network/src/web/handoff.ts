/**
 * 手机扫码入口：把当前浏览器身份通过 URL fragment 交给手机。
 *
 * token 只出现在 `#t=` 之后；浏览器不会把 fragment 发给服务端，因此
 * 二维码不经过任何服务端端点，也不引入新工具、密钥或数据库状态。
 * 代价是二维码本身就是长期凭据：任何看到二维码的人都能获得同一身份。
 */

/** 左下角用户区块：头像、名称与“手机扫码”入口。 */
export function deviceHandoffAccount(): string {
  return `<div class="account" id="account">
    <span class="account-avatar" id="account-avatar" aria-hidden="true">?</span>
    <div class="account-main">
      <span class="account-name" id="account-name">未登录</span>
      <button class="account-scan" id="device-handoff" type="button"
        aria-haspopup="dialog" aria-controls="handoff-modal" aria-expanded="false">手机扫码</button>
    </div>
  </div>`;
}

/** 扫码弹窗；不显示“已扫码”，服务端无感知。 */
export function deviceHandoffDialog(): string {
  return `<div class="modal" id="handoff-modal" hidden>
  <div class="modal-card handoff-card" role="dialog" aria-modal="true" aria-labelledby="handoff-title">
    <h2 id="handoff-title">手机扫码</h2>
    <p class="handoff-hint" id="handoff-hint">用手机相机扫描二维码，手机将以当前身份进入会话列表。</p>
    <div class="handoff-qr-wrap">
      <canvas id="handoff-qr" role="img" aria-label="包含当前登录凭据的二维码"></canvas>
    </div>
    <p class="handoff-warning">二维码包含当前身份的完整登录凭据，请勿截图或分享。任何人看到二维码都能以你的身份操作；凭据泄露后需在服务端撤销该 token。</p>
    <p class="handoff-hint">二维码在凭据被撤销前一直可用；页面不会显示“已扫码”状态。</p>
    <div class="handoff-actions">
      <button class="primary" id="handoff-close" type="button">关闭</button>
    </div>
  </div>
</div>`;
}

/**
 * 在页面主脚本之前执行：采纳 `#t=` 中的 token 并清掉 hash。
 *
 * 必须先于会话列表加载执行，因此由 `renderAppPage` 放在 `<main>` 开头，
 * 解析到该 `<script>` 时立即运行。
 */
export const HANDOFF_ADOPT_SCRIPT = `
(function () {
  'use strict';
  var hash = window.location.hash || '';
  if (hash.indexOf('#t=') !== 0) return;
  var token;
  try {
    token = decodeURIComponent(hash.slice(3));
  } catch (error) {
    return;
  }
  if (!token || token.length > 512 || token.trim() !== token) return;
  try {
    window.localStorage.setItem('agent-network-token', token);
  } catch (error) {
    return;
  }
  try {
    window.history.replaceState(null, '', window.location.pathname + window.location.search);
  } catch (error) {
    // 清 hash 失败不影响身份采纳；下次打开会再次写入同一 token。
  }
})();
`;

/** 弹窗控制：从 localStorage 读 token，现场生成二维码，不请求服务端。 */
export const HANDOFF_CONTROLLER_SCRIPT = `
(function () {
  'use strict';
  var TOKEN_KEY = 'agent-network-token';
  var button = document.getElementById('device-handoff');
  var modal = document.getElementById('handoff-modal');
  var canvas = document.getElementById('handoff-qr');
  var closeButton = document.getElementById('handoff-close');
  var hint = document.getElementById('handoff-hint');
  if (!button || !modal || !canvas || !closeButton || !hint) return;
  var opener = null;

  function closeDialog() {
    modal.hidden = true;
    button.setAttribute('aria-expanded', 'false');
    if (opener && opener.focus) opener.focus();
  }

  function openDialog() {
    var token = null;
    try {
      token = window.localStorage.getItem(TOKEN_KEY);
    } catch (error) {
      token = null;
    }
    if (!token) {
      canvas.hidden = true;
      hint.textContent = '当前浏览器还没有身份。先打开分享链接完成建联，再扫码。';
    } else {
      var url = window.location.origin + '/#t=' + encodeURIComponent(token);
      try {
        window.agentNetworkQr.draw(canvas, url);
        canvas.hidden = false;
        hint.textContent = '用手机相机扫描二维码，手机将以当前身份进入会话列表。';
      } catch (error) {
        canvas.hidden = true;
        hint.textContent = '二维码生成失败：' + (error && error.message ? error.message : String(error));
      }
    }
    opener = document.activeElement;
    modal.hidden = false;
    button.setAttribute('aria-expanded', 'true');
    closeButton.focus();
  }

  button.addEventListener('click', openDialog);
  closeButton.addEventListener('click', closeDialog);
  modal.addEventListener('click', function (event) {
    if (event.target === modal) closeDialog();
  });
  document.addEventListener('keydown', function (event) {
    if (event.key === 'Escape' && !modal.hidden) closeDialog();
  });
})();
`;

/** 追加到应用页样式表；二维码保持白底，暗色模式下仍可扫描。 */
export const DEVICE_HANDOFF_STYLE = `
  .account { display: flex; align-items: center; gap: 10px; padding: 4px 4px 8px; }
  .account-avatar { flex: 0 0 auto; width: 36px; height: 36px; border-radius: 50%;
    background: #5b8def; color: #fff; display: grid; place-items: center;
    font-size: 15px; font-weight: 500; }
  .account-main { flex: 1 1 auto; min-width: 0; }
  .account-name { display: block; font-size: 13px; font-weight: 500; color: var(--wx-text, #333);
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .account-scan { margin-top: 1px; padding: 0; border: 0; background: none;
    color: var(--wx-link, #576b95); font-size: 12px; cursor: pointer; }
  .account-scan:hover { text-decoration: underline; }
  .handoff-card { width: min(340px, 100%); text-align: center; }
  .handoff-card h2 { margin-bottom: 6px; }
  .handoff-qr-wrap { display: grid; place-items: center; margin: 10px 0 12px; padding: 8px;
    border: 1px solid var(--wx-line, #e5e5e5); border-radius: 6px; background: #fff; }
  #handoff-qr { display: block; width: min(248px, 100%); height: auto; background: #fff;
    image-rendering: pixelated; }
  #handoff-qr[hidden] { display: none; }
  .handoff-hint { margin: 0 0 8px; font-size: 12px; color: var(--wx-muted, #555);
    line-height: 1.6; }
  .handoff-warning { margin: 0 0 12px; padding: 8px 10px; border-radius: 4px;
    background: #fff7ed; color: #9a3412; font-size: 12px; line-height: 1.6; text-align: left; }
  .handoff-actions { display: flex; }
  .handoff-actions button { flex: 1 1 auto; height: 40px; }
  @media (prefers-color-scheme: dark) {
    .handoff-warning { background: #3a2a1a; color: #fdba74; }
  }
`;
