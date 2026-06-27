// ========== Getman - API 测试面板 ==========

(function() {

// ---------- 常量 ----------
var HTTP_METHODS = ["GET","POST","PUT","PATCH","DELETE","HEAD","OPTIONS"];
var METHOD_COLORS = {
  GET:    "#0c9",  POST:   "#09c",
  PUT:    "#c80",  PATCH:  "#c6a",
  DELETE: "#c33",  HEAD:   "#888",
  OPTIONS: "#888",
};
var BODY_TYPES = ["none", "json", "form-data", "x-www-form-urlencoded"];

var CONTENT_TYPE_MAP = {
  json:                 "application/json",
  "form-data":          "multipart/form-data",
  "x-www-form-urlencoded": "application/x-www-form-urlencoded",
};

var HISTORY_KEY = "getman:history";
var MAX_HISTORY = 50;

// ---------- 状态 ----------
var state = {
  method: "GET",
  url: "",
  params: [],    // [{key,value,enabled}]
  headers: [
    { key: "Accept",       value: "*/*",         enabled: true },
    { key: "User-Agent",   value: "Getman/1.0",  enabled: true },
  ],
  bodyType: "none",
  bodyContent: "",  // raw body text
  formFields: [],   // [{key,value,enabled}] for form-data / x-www-form-urlencoded
  auth: { type: "none", bearer: "", basicUser: "", basicPass: "" },
  // 响应
  response: null,    // {status,statusText,time,size,headers:{},body,contentType}
  responseTab: "body", // body | headers
  // UI
  activeTab: "params", // params | headers | body | auth
  historyOpen: false,
};

var historyList = []; // [{method,url,params,headers,bodyType,bodyContent,formFields,auth,timestamp}]

// ---------- DOM 引用 ----------
var overlay, backdrop, panel, headerEl, bodyEl;
var methodSelect, urlInput, sendBtn, saveBtn;
var tabsContainer, tabPanels;
var kvEditors = {}; // {params, headers, formFields} -> container el
var bodyTypeSelect, bodyTextarea, formEditorContainer;
var authTypeSelect, authFields;
var responseEl, responseStatus, responseTime, responseSize, responseHeadersEl, responseBodyEl;
var historyToggle, historyPanel, historyListEl;
var resizeHandle;
var importBtn, importPanel, importTextarea, importParseBtn, importCancelBtn, importResultEl;

// ---------- 初始化 ----------
function init() {
  overlay         = document.getElementById("getman-overlay");
  backdrop        = overlay.querySelector(".getman-overlay-backdrop");
  panel           = document.getElementById("getman-overlay-panel");
  headerEl        = document.getElementById("getman-overlay-header");
  bodyEl          = overlay.querySelector(".getman-overlay-body");
  methodSelect    = document.getElementById("getman-method");
  urlInput        = document.getElementById("getman-url");
  sendBtn         = document.getElementById("getman-send");
  saveBtn         = document.getElementById("getman-save");
  tabsContainer   = document.getElementById("getman-tabs");
  bodyTypeSelect  = document.getElementById("getman-body-type");
  bodyTextarea    = document.getElementById("getman-body-raw");
  authTypeSelect  = document.getElementById("getman-auth-type");
  responseEl      = document.getElementById("getman-response");
  historyToggle   = document.getElementById("getman-history-toggle");
  historyPanel    = document.getElementById("getman-history-panel");
  historyListEl   = document.getElementById("getman-history-list");
  resizeHandle    = document.getElementById("getman-overlay-resize");
  importBtn       = document.getElementById("getman-import-btn");
  importPanel     = document.getElementById("getman-import-panel");
  importTextarea  = document.getElementById("getman-import-textarea");
  importParseBtn  = document.getElementById("getman-import-parse");
  importCancelBtn = document.getElementById("getman-import-cancel");
  importResultEl  = document.getElementById("getman-import-result");

  // KV 编辑器
  kvEditors.params    = document.getElementById("getman-kv-params");
  kvEditors.headers   = document.getElementById("getman-kv-headers");
  kvEditors.formFields = document.getElementById("getman-kv-form");

  // Auth 字段
  authFields = {
    bearer:     document.getElementById("getman-auth-bearer"),
    basicUser:  document.getElementById("getman-auth-basic-user"),
    basicPass:  document.getElementById("getman-auth-basic-pass"),
  };

  // Tab 面板
  tabPanels = {
    params:  document.getElementById("getman-tab-params"),
    headers: document.getElementById("getman-tab-headers"),
    body:    document.getElementById("getman-tab-body"),
    auth:    document.getElementById("getman-tab-auth"),
  };

  // Response 子面板
  responseStatus   = document.getElementById("getman-res-status");
  responseTime     = document.getElementById("getman-res-time");
  responseSize     = document.getElementById("getman-res-size");
  responseHeadersEl = document.getElementById("getman-res-headers");
  responseBodyEl   = document.getElementById("getman-res-body");

  bindEvents();
  loadHistory();
  updateMethodColor();
}

// ---------- 事件绑定 ----------
function bindEvents() {
  // 关闭
  document.getElementById("getman-overlay-close").onclick = close;
  backdrop.onclick = close;

  // Method 选择
  methodSelect.onchange = function() {
    state.method = methodSelect.value;
    updateMethodColor();
    // GET/HEAD 不允许 body
    if (state.method === "GET" || state.method === "HEAD") {
      if (state.bodyType !== "none") {
        state.bodyType = "none";
        bodyTypeSelect.value = "none";
        renderBodyEditor();
      }
      disableBodyOption(true);
    } else {
      disableBodyOption(false);
    }
  };

  // URL 输入
  urlInput.oninput = function() { state.url = urlInput.value; };

  // Ctrl+V 粘贴 cURL → 自动解析（Postman 风格）
  urlInput.addEventListener("paste", function(e) {
    var text = (e.clipboardData || window.clipboardData).getData("text").trim();
    if (!text || !/^\s*curl\b/i.test(text)) return;

    e.preventDefault();
    importResultEl.innerHTML = "";

    fetch("/api/getman/parse-curl", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ curl: text }),
    })
    .then(function(resp) { return resp.json(); })
    .then(function(parsed) {
      if (parsed.error) {
        importResultEl.innerHTML = '<div class="getman-import-error">' + escapeHtml(parsed.error) + '</div>';
        urlInput.value = text;
        state.url = text;
        return;
      }
      applyParsedCurl(parsed);
    })
    .catch(function(err) {
      urlInput.value = text;
      state.url = text;
    });
  });

  // Send
  sendBtn.onclick = sendRequest;

  // Save
  saveBtn.onclick = saveToHistory;

  // 快捷键: Ctrl/Cmd+Enter → 发送
  urlInput.onkeydown = function(e) {
    if ((e.ctrlKey || e.metaKey) && e.key === "Enter") {
      e.preventDefault();
      sendRequest();
    }
  };

  // Tab 切换
  tabsContainer.onclick = function(e) {
    var tab = e.target.closest(".getman-tab");
    if (!tab) return;
    var tabName = tab.dataset.tab;
    switchTab(tabName);
  };

  // Body 类型
  bodyTypeSelect.onchange = function() {
    state.bodyType = bodyTypeSelect.value;
    renderBodyEditor();
  };

  // Auth 类型
  authTypeSelect.onchange = function() {
    state.auth.type = authTypeSelect.value;
    renderAuthFields();
  };

  // Import cURL
  importBtn.onclick = toggleImport;
  importCancelBtn.onclick = function() { hideImport(); };
  importParseBtn.onclick = parseAndImport;

  // Response tab 切换
  responseEl.onclick = function(e) {
    var tab = e.target.closest(".getman-res-tab");
    if (!tab) return;
    state.responseTab = tab.dataset.restab;
    renderResponseTabs();
  };

  // 历史面板
  historyToggle.onclick = toggleHistory;

  // Esc 关闭：先关 import，再关面板
  document.addEventListener("keydown", function(e) {
    if (e.key === "Escape" && overlay.style.display !== "none") {
      if (importPanel.style.display === "block") {
        hideImport();
        return;
      }
      close();
    }
    // Ctrl+I → 打开 import
    if ((e.ctrlKey || e.metaKey) && e.key === "i" && overlay.style.display !== "none") {
      e.preventDefault();
      toggleImport();
    }
  });

  // 窗口大小调整
  setupResize();
}

// ---------- 打开 / 关闭 ----------
function open() {
  overlay.style.display = "block";
  urlInput.focus();
}

function close() {
  overlay.style.display = "none";
}

window._getmanOpen = open;

// ---------- Method 颜色 ----------
function updateMethodColor() {
  var color = METHOD_COLORS[state.method] || METHOD_COLORS.GET;
  methodSelect.style.color = color;
  methodSelect.style.fontWeight = "700";
}

// ---------- Tab 切换 ----------
function switchTab(name) {
  state.activeTab = name;

  var tabs = tabsContainer.querySelectorAll(".getman-tab");
  for (var i = 0; i < tabs.length; i++) {
    tabs[i].classList.toggle("active", tabs[i].dataset.tab === name);
  }

  Object.keys(tabPanels).forEach(function(k) {
    tabPanels[k].style.display = (k === name) ? "flex" : "none";
  });
}

// ---------- KV 编辑器渲染 ----------
function renderKVEditor(containerId, items, onChange, typeLabel) {
  var container = kvEditors[containerId];
  if (!container) return;

  var html = '<div class="getman-kv-list">';
  for (var i = 0; i < items.length; i++) {
    var item = items[i];
    var chkId = "kv-" + containerId + "-" + i;
    html += '<div class="getman-kv-row">' +
      '<input type="checkbox" class="getman-kv-check"' +
        (item.enabled !== false ? " checked" : "") +
        ' data-idx="' + i + '" data-field="enabled" id="' + chkId + '">' +
      '<input type="text" class="getman-kv-key" placeholder="Key"' +
        ' value="' + escapeHtml(item.key) + '" data-idx="' + i + '" data-field="key">' +
      '<input type="text" class="getman-kv-value" placeholder="Value"' +
        ' value="' + escapeHtml(item.value) + '" data-idx="' + i + '" data-field="value">' +
      '<button class="getman-kv-remove" data-idx="' + i + '" title="移除">✕</button>' +
      '</div>';
  }
  html += '</div>' +
    '<button class="getman-kv-add" data-container="' + containerId + '">+ Add ' + typeLabel + '</button>';

  container.innerHTML = html;

  // 事件委托
  container.onclick = function(e) {
    var addBtn = e.target.closest(".getman-kv-add");
    if (addBtn) {
      e.preventDefault();
      items.push({ key: "", value: "", enabled: true });
      renderKVEditor(containerId, items, onChange, typeLabel);
      if (onChange) onChange();
      return;
    }

    var removeBtn = e.target.closest(".getman-kv-remove");
    if (removeBtn) {
      e.preventDefault();
      var idx = parseInt(removeBtn.dataset.idx);
      items.splice(idx, 1);
      renderKVEditor(containerId, items, onChange, typeLabel);
      if (onChange) onChange();
      return;
    }
  };

  container.oninput = function(e) {
    var input = e.target.closest("input[data-idx]");
    if (!input) return;
    var idx = parseInt(input.dataset.idx);
    var field = input.dataset.field;
    var value = input.type === "checkbox" ? input.checked : input.value;
    if (idx >= 0 && idx < items.length) {
      items[idx][field] = value;
    }
    if (onChange) onChange();
  };
}

// ---------- Body 编辑器 ----------
function renderBodyEditor() {
  // 原始文本区域
  bodyTextarea.style.display = (state.bodyType === "json" || state.bodyType === "none") ? "block" : "none";

  // Form 编辑器
  var formContainer = document.getElementById("getman-body-form-editor");
  var isForm = (state.bodyType === "form-data" || state.bodyType === "x-www-form-urlencoded");
  formContainer.style.display = isForm ? "block" : "none";

  if (state.bodyType === "json") {
    bodyTextarea.placeholder = '{\n  "key": "value"\n}';
  } else if (state.bodyType === "none") {
    bodyTextarea.placeholder = "Body not supported for " + state.method;
  }

  if (isForm) {
    renderKVEditor("formFields", state.formFields, null, "Field");
  }
}

function disableBodyOption(disabled) {
  var opts = bodyTypeSelect.options;
  var noneOpt = null;
  for (var i = 0; i < opts.length; i++) {
    if (opts[i].value === "none") { noneOpt = i; continue; }
    opts[i].disabled = disabled;
  }
  if (disabled && noneOpt !== null) {
    bodyTypeSelect.value = "none";
    state.bodyType = "none";
    renderBodyEditor();
  }
}

// ---------- Auth 字段 ----------
function renderAuthFields() {
  var t = state.auth.type;
  document.getElementById("getman-auth-bearer-row").style.display = (t === "bearer") ? "flex" : "none";
  document.getElementById("getman-auth-basic-row").style.display  = (t === "basic")  ? "flex" : "none";

  if (t === "bearer") {
    authFields.bearer.value = state.auth.bearer;
    authFields.bearer.oninput = function() { state.auth.bearer = authFields.bearer.value; };
  }
  if (t === "basic") {
    authFields.basicUser.value = state.auth.basicUser;
    authFields.basicPass.value = state.auth.basicPass;
    authFields.basicUser.oninput = function() { state.auth.basicUser = authFields.basicUser.value; };
    authFields.basicPass.oninput = function() { state.auth.basicPass = authFields.basicPass.value; };
  }
}

// ---------- 构建请求头 ----------
function buildHeaders() {
  var hdrs = {};
  for (var i = 0; i < state.headers.length; i++) {
    var h = state.headers[i];
    if (h.enabled !== false && h.key) {
      hdrs[h.key] = h.value;
    }
  }
  // Body content-type
  if (state.bodyType !== "none" && CONTENT_TYPE_MAP[state.bodyType]) {
    if (!hdrs["Content-Type"]) {
      hdrs["Content-Type"] = CONTENT_TYPE_MAP[state.bodyType];
    }
  }
  // Auth
  if (state.auth.type === "bearer" && state.auth.bearer) {
    hdrs["Authorization"] = "Bearer " + state.auth.bearer;
  } else if (state.auth.type === "basic" && state.auth.basicUser) {
    hdrs["Authorization"] = "Basic " + btoa(state.auth.basicUser + ":" + state.auth.basicPass);
  }
  return hdrs;
}

// ---------- 构建请求体 ----------
function buildBody() {
  if (state.bodyType === "none") return null;
  if (state.bodyType === "json") return state.bodyContent || "";
  if (state.bodyType === "x-www-form-urlencoded") {
    var parts = [];
    for (var i = 0; i < state.formFields.length; i++) {
      var f = state.formFields[i];
      if (f.enabled !== false && f.key) {
        parts.push(encodeURIComponent(f.key) + "=" + encodeURIComponent(f.value));
      }
    }
    return parts.join("&");
  }
  // form-data: 用 FormData → 但 proxy 是 JSON，所以传 fields 数组
  return null;
}

function buildFormFields() {
  if (state.bodyType !== "form-data") return null;
  return state.formFields.filter(function(f) { return f.enabled !== false && f.key; });
}

// ---------- 发送请求 ----------
async function sendRequest() {
  var url = state.url.trim();
  if (!url) {
    showResponse({ status: 0, statusText: "请输入 URL", time: 0, size: 0, headers: {}, body: "", contentType: "" });
    return;
  }

  // 构建查询参数
  if (state.params.length > 0) {
    var activeParams = state.params.filter(function(p) { return p.enabled !== false && p.key; });
    if (activeParams.length > 0) {
      var sep = url.indexOf("?") >= 0 ? "&" : "?";
      var qs = activeParams.map(function(p) {
        return encodeURIComponent(p.key) + "=" + encodeURIComponent(p.value);
      }).join("&");
      url += sep + qs;
    }
  }

  // 标记加载
  sendBtn.disabled = true;
  sendBtn.textContent = "Sending...";

  var startTime = performance.now();

  try {
    var resp = await fetch("/api/getman/proxy", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        method:  state.method,
        url:     url,
        headers: buildHeaders(),
        body:    buildBody(),
        formFields: buildFormFields(),
      }),
    });

    var endTime = performance.now();
    var elapsed = Math.round(endTime - startTime);

    // 读取响应
    var proxyResult;
    var ct = resp.headers.get("Content-Type") || "";
    if (ct.includes("application/json")) {
      proxyResult = await resp.json();
    } else {
      var text = await resp.text();
      try { proxyResult = JSON.parse(text); } catch(e) { proxyResult = { error: "解析响应失败", raw: text }; }
    }

    if (proxyResult.error && !proxyResult.status) {
      showResponse({
        status: 0,
        statusText: proxyResult.error,
        time: elapsed,
        size: 0,
        headers: {},
        body: proxyResult.detail || "",
        contentType: "text/plain",
      });
    } else {
      showResponse({
        status: proxyResult.status || 0,
        statusText: proxyResult.statusText || "",
        time: elapsed,
        size: proxyResult.size || 0,
        headers: proxyResult.headers || {},
        body: proxyResult.body || "",
        contentType: proxyResult.contentType || "text/plain",
      });
    }

    // 自动存历史
    autoSaveHistory();

  } catch (err) {
    var endTime = performance.now();
    showResponse({
      status: 0,
      statusText: "Network Error",
      time: Math.round(endTime - startTime),
      size: 0,
      headers: {},
      body: err.message || String(err),
      contentType: "text/plain",
    });
  }

  sendBtn.disabled = false;
  sendBtn.textContent = "Send";
}

// ---------- 显示响应 ----------
function showResponse(res) {
  state.response = res;
  responseEl.style.display = "flex";

  // 状态码
  var statusClass = res.status >= 200 && res.status < 300 ? "getman-status-ok"
    : res.status >= 300 && res.status < 400 ? "getman-status-redirect"
    : res.status >= 400 ? "getman-status-error"
    : "getman-status-neutral";

  responseStatus.className = "getman-res-status-val " + statusClass;
  responseStatus.textContent = res.status ? (res.status + " " + res.statusText) : res.statusText;

  // 时间
  responseTime.textContent = res.time + "ms";

  // 大小
  if (res.size > 1024) {
    responseSize.textContent = (res.size / 1024).toFixed(1) + " KB";
  } else {
    responseSize.textContent = res.size + " B";
  }

  // Headers
  var headersHtml = '<table class="getman-res-headers-table">';
  var keys = Object.keys(res.headers).sort();
  for (var i = 0; i < keys.length; i++) {
    headersHtml += '<tr><td class="getman-res-header-key">' + escapeHtml(keys[i]) +
      '</td><td class="getman-res-header-val">' + escapeHtml(res.headers[keys[i]]) + '</td></tr>';
  }
  headersHtml += '</table>';
  responseHeadersEl.innerHTML = headersHtml;

  // Body
  renderResponseBody(res);

  // Tab
  renderResponseTabs();
}

function renderResponseBody(res) {
  res = res || state.response;
  if (!res) return;

  var body = res.body || "";
  var ct = res.contentType || "";

  // 尝试格式化 JSON
  if (ct.includes("json") || ct.includes("javascript")) {
    try {
      var parsed = typeof body === "string" ? JSON.parse(body) : body;
      body = JSON.stringify(parsed, null, 2);
    } catch(e) { /* keep as-is */ }
    responseBodyEl.className = "getman-res-body-content getman-res-body-json";
    responseBodyEl.innerHTML = '<pre>' + escapeHtml(body) + '</pre>';
  } else if (ct.includes("html")) {
    responseBodyEl.className = "getman-res-body-content getman-res-body-html";
    responseBodyEl.innerHTML = '<pre>' + escapeHtml(body) + '</pre>';
  } else if (ct.includes("xml")) {
    responseBodyEl.className = "getman-res-body-content getman-res-body-xml";
    responseBodyEl.innerHTML = '<pre>' + escapeHtml(body) + '</pre>';
  } else {
    responseBodyEl.className = "getman-res-body-content getman-res-body-text";
    responseBodyEl.innerHTML = '<pre>' + escapeHtml(body) + '</pre>';
  }
}

function renderResponseTabs() {
  var tabs = responseEl.querySelectorAll(".getman-res-tab");
  for (var i = 0; i < tabs.length; i++) {
    tabs[i].classList.toggle("active", tabs[i].dataset.restab === state.responseTab);
  }
  document.getElementById("getman-res-body-panel").style.display = (state.responseTab === "body") ? "block" : "none";
  document.getElementById("getman-res-headers-panel").style.display = (state.responseTab === "headers") ? "block" : "none";
}

// ---------- 历史 ----------
function autoSaveHistory() {
  var entry = {
    method:      state.method,
    url:         state.url,
    params:      JSON.parse(JSON.stringify(state.params)),
    headers:     JSON.parse(JSON.stringify(state.headers)),
    bodyType:    state.bodyType,
    bodyContent: state.bodyContent,
    formFields:  JSON.parse(JSON.stringify(state.formFields)),
    auth:        JSON.parse(JSON.stringify(state.auth)),
    timestamp:   Date.now(),
  };
  // 去重（相同 URL+Method 的仅保留最新）
  historyList = historyList.filter(function(h) {
    return !(h.method === entry.method && h.url === entry.url);
  });
  historyList.unshift(entry);
  if (historyList.length > MAX_HISTORY) historyList = historyList.slice(0, MAX_HISTORY);
  persistHistory();
  renderHistoryList();
}

function saveToHistory() {
  if (!state.url.trim()) return;
  autoSaveHistory();
  // 闪烁提示
  saveBtn.style.color = "var(--accent)";
  setTimeout(function() { saveBtn.style.color = ""; }, 600);
}

function loadHistory() {
  try {
    var raw = localStorage.getItem(HISTORY_KEY);
    if (raw) historyList = JSON.parse(raw);
  } catch(e) { historyList = []; }
}

function persistHistory() {
  try {
    localStorage.setItem(HISTORY_KEY, JSON.stringify(historyList));
  } catch(e) { /* quota exceeded, ignore */ }
}

function renderHistoryList() {
  if (historyList.length === 0) {
    historyListEl.innerHTML = '<div class="getman-history-empty">暂无历史请求</div>';
    return;
  }

  var html = '';
  for (var i = 0; i < historyList.length; i++) {
    var h = historyList[i];
    var color = METHOD_COLORS[h.method] || "#888";
    var shortUrl = h.url.length > 40 ? h.url.slice(0, 40) + "..." : h.url;
    html += '<div class="getman-history-item" data-idx="' + i + '">' +
      '<span class="getman-history-method" style="color:' + color + '">' + escapeHtml(h.method) + '</span>' +
      '<span class="getman-history-url">' + escapeHtml(shortUrl) + '</span>' +
      '<button class="getman-history-del" data-idx="' + i + '" title="删除">✕</button>' +
      '</div>';
  }

  html += '<div id="getman-history-clear" class="getman-history-clear">清空历史</div>';
  historyListEl.innerHTML = html;

  // 点击加载
  historyListEl.onclick = function(e) {
    var delBtn = e.target.closest(".getman-history-del");
    if (delBtn) {
      e.stopPropagation();
      var idx = parseInt(delBtn.dataset.idx);
      historyList.splice(idx, 1);
      persistHistory();
      renderHistoryList();
      return;
    }

    var clearBtn = e.target.closest("#getman-history-clear");
    if (clearBtn) {
      historyList = [];
      persistHistory();
      renderHistoryList();
      return;
    }

    var item = e.target.closest(".getman-history-item");
    if (!item) return;
    var idx = parseInt(item.dataset.idx);
    loadHistoryEntry(historyList[idx]);
  };
}

function loadHistoryEntry(entry) {
  state.method     = entry.method;
  state.url        = entry.url;
  state.params     = JSON.parse(JSON.stringify(entry.params || []));
  state.headers    = JSON.parse(JSON.stringify(entry.headers || []));
  state.bodyType   = entry.bodyType || "none";
  state.bodyContent = entry.bodyContent || "";
  state.formFields = JSON.parse(JSON.stringify(entry.formFields || []));
  state.auth       = JSON.parse(JSON.stringify(entry.auth || { type: "none" }));
  state.response   = null;

  methodSelect.value      = state.method;
  urlInput.value          = state.url;
  bodyTypeSelect.value    = state.bodyType;
  bodyTextarea.value      = state.bodyContent;
  authTypeSelect.value    = state.auth.type;

  updateMethodColor();
  renderKVEditor("params", state.params, null, "Param");
  renderKVEditor("headers", state.headers, null, "Header");
  renderBodyEditor();
  renderAuthFields();
  responseEl.style.display = "none";

  // 关闭历史面板
  if (state.historyOpen) toggleHistory();
}

function toggleHistory() {
  state.historyOpen = !state.historyOpen;
  historyPanel.classList.toggle("open", state.historyOpen);
  historyToggle.classList.toggle("active", state.historyOpen);
  if (state.historyOpen) {
    renderHistoryList();
  }
}

// ---------- cURL 导入 ----------
function toggleImport() {
  var visible = importPanel.style.display === "block";
  if (visible) {
    hideImport();
  } else {
    importPanel.style.display = "block";
    importTextarea.value = "";
    importResultEl.innerHTML = "";
    importTextarea.focus();
  }
}

function hideImport() {
  importPanel.style.display = "none";
}

// cURL 解析由服务端 /api/getman/parse-curl 处理（使用 parse-curl 库）

// 共用：将解析结果写入 UI
function applyParsedCurl(parsed) {
  state.method      = parsed.method;
  state.url         = parsed.url;
  state.params      = parsed.params || [];
  state.headers     = (parsed.headers && parsed.headers.length > 0) ? parsed.headers : [
    { key: "Accept", value: "*/*", enabled: true },
    { key: "User-Agent", value: "Getman/1.0", enabled: true },
  ];
  state.bodyType    = parsed.bodyType || "none";
  state.bodyContent = parsed.body || "";
  state.formFields  = parsed.formFields || [];
  state.auth = {
    type: parsed.authType || "none",
    bearer: parsed.authBearer || "",
    basicUser: parsed.authBasicUser || "",
    basicPass: parsed.authBasicPass || "",
  };
  state.response    = null;

  methodSelect.value   = state.method;
  urlInput.value       = state.url;
  bodyTypeSelect.value = state.bodyType;
  bodyTextarea.value   = state.bodyContent;
  authTypeSelect.value = state.auth.type;

  updateMethodColor();
  renderKVEditor("params", state.params, null, "Param");
  renderKVEditor("headers", state.headers, null, "Header");
  renderBodyEditor();
  renderAuthFields();

  if (state.method === "GET" || state.method === "HEAD") {
    disableBodyOption(true);
  } else {
    disableBodyOption(false);
  }

  responseEl.style.display = "none";

  var shortUrl = state.url.length > 50 ? state.url.slice(0, 50) + "..." : state.url;
  var summary = parsed.method + " " + shortUrl +
    (parsed.headers && parsed.headers.length > 0 ? " · " + parsed.headers.length + " headers" : "") +
    (parsed.body ? " · body" : "") +
    (parsed.authType && parsed.authType !== "none" ? " · " + parsed.authType : "");
  importResultEl.innerHTML = '<div class="getman-import-ok">✓ ' + escapeHtml(summary) + '</div>';

  setTimeout(function() { hideImport(); }, 1500);
}

function parseAndImport() {
  var raw = importTextarea.value.trim();
  if (!raw) {
    importResultEl.innerHTML = '<div class="getman-import-error">请粘贴 curl 命令</div>';
    return;
  }

  importParseBtn.disabled = true;
  importParseBtn.textContent = "Parsing...";

  fetch("/api/getman/parse-curl", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ curl: raw }),
  })
  .then(function(resp) { return resp.json(); })
  .then(function(parsed) {
    importParseBtn.disabled = false;
    importParseBtn.textContent = "Parse & Import";
    if (parsed.error) {
      importResultEl.innerHTML = '<div class="getman-import-error">' + escapeHtml(parsed.error) + '</div>';
      return;
    }
    applyParsedCurl(parsed);
  })
  .catch(function(err) {
    importParseBtn.disabled = false;
    importParseBtn.textContent = "Parse & Import";
    importResultEl.innerHTML = '<div class="getman-import-error">请求失败: ' + escapeHtml(err.message || String(err)) + '</div>';
  });
}

// ---------- 窗口大小调整 ----------
function setupResize() {
  var isDragging = false;
  var startX, startY, startW, startH;

  resizeHandle.onmousedown = function(e) {
    isDragging = true;
    startX = e.clientX;
    startY = e.clientY;
    startW = panel.offsetWidth;
    startH = panel.offsetHeight;
    document.body.style.userSelect = "none";
    e.preventDefault();
  };

  document.addEventListener("mousemove", function(e) {
    if (!isDragging) return;
    var dx = e.clientX - startX;
    var dy = e.clientY - startY;
    panel.style.width  = Math.max(480, startW + dx) + "px";
    panel.style.height = Math.max(320, startH + dy) + "px";
  });

  document.addEventListener("mouseup", function() {
    if (!isDragging) return;
    isDragging = false;
    document.body.style.userSelect = "";
  });
}

// ---------- 工具函数 ----------
function escapeHtml(s) {
  if (!s) return "";
  return String(s)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

// ---------- 初始化渲染 ----------
function initialRender() {
  methodSelect.innerHTML = HTTP_METHODS.map(function(m) {
    return '<option value="' + m + '"' + (m === state.method ? " selected" : "") + '>' + m + '</option>';
  }).join("");

  bodyTypeSelect.innerHTML = BODY_TYPES.map(function(t) {
    return '<option value="' + t + '"' + (t === state.bodyType ? " selected" : "") + '>' + t + '</option>';
  }).join("");

  updateMethodColor();
  renderKVEditor("params", state.params, null, "Param");
  renderKVEditor("headers", state.headers, null, "Header");
  renderBodyEditor();
  renderAuthFields();
  renderHistoryList();

  // 监听 body textarea
  bodyTextarea.oninput = function() { state.bodyContent = bodyTextarea.value; };
}

// ---------- 启动 ----------
if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", function() { init(); initialRender(); });
} else {
  init();
  initialRender();
}

})();
