(function() { "use strict";

window.originalOnPageActivate2 = window.onPageActivate;
window.onPageActivate = function(pageName) {
  if (window.originalOnPageActivate2) window.originalOnPageActivate2(pageName);
  if (pageName === "threads") { fetchFilters(); loadThreads(); }
};

var threadPage = 1;
var threadSearch = "";
var threadStatus = "";
var threadCwd = "";
var threadSort = "updated_at";
var threadOrder = "DESC";
var threadHideEmpty = true; // 默认过滤空会话

// ── 加载筛选项 ──
async function fetchFilters() {
  try {
    var data = await window.api.get("/api/stats");
    if (!data.error) {
      // 状态下拉
      var sel = document.getElementById("thread-status-filter");
      var current = sel.value;
      sel.innerHTML = '<option value="">All Status</option>';
      (data.agentStatusDist || []).forEach(function(s) {
        sel.innerHTML += '<option value="' + escHtml(s.agent_status) + '">' +
          escHtml(s.agent_status) + ' (' + s.count + ')</option>';
      });
      sel.value = current;
    }
    // cwd 下拉
    var cwds = await window.api.get("/api/cwds");
    if (!cwds.error) {
      var csel = document.getElementById("thread-cwd-filter");
      var currentCwd = csel.value;
      csel.innerHTML = '<option value="">All CWD</option>';
      (cwds || []).forEach(function(c) {
        var short = c.length > 60 ? "..." + c.slice(-60) : c;
        csel.innerHTML += '<option value="' + escHtml(c) + '">' + escHtml(short) + '</option>';
      });
      csel.value = currentCwd;
    }
  } catch (e) {}
}

// ── Esc HTML ──
function escHtml(s) {
  if (!s) return "";
  return String(s).replace(/&/g,"&amp;").replace(/</g,"&lt;").replace(/>/g,"&gt;").replace(/"/g,"&quot;");
}

// ── 加载线程列表 ──
async function loadThreads() {
  var params = new URLSearchParams({
    page: threadPage, perPage: 50,
    sort: threadSort, order: threadOrder,
  });
  if (threadStatus) params.set("status", threadStatus);
  if (threadSearch) params.set("search", threadSearch);
  if (threadCwd) params.set("cwd", threadCwd);
  if (threadHideEmpty) params.set("minMsg", "1");

  try {
    var data = await window.api.get("/api/threads?" + params.toString());
    if (data.error) return;
    renderThreadTable(data.rows);
    renderPagination(data.total, data.page, data.perPage);
  } catch (e) { console.error(e); }
}

function renderThreadTable(rows) {
  var tbody = document.getElementById("threads-tbody");
  tbody.innerHTML = (rows || []).map(function(t) {
    var title = t.title || "(untitled)";
    var subAgents = t.subagent_count != null ? t.subagent_count : "-";
    return '<tr>' +
      '<td><span class="thread-id-link" data-id="' + escHtml(t.id) + '">' +
        escHtml(t.id.substring(0, 8)) + '...</span></td>' +
      '<td>' + escHtml(window.api.truncate(title, 60)) + '</td>' +
      '<td><span class="badge badge-info">' + escHtml(t.agent_status || "-") + '</span></td>' +
      '<td>' + t.message_count + '</td>' +
      '<td>' + window.api.formatDate(t.created_at) + '</td>' +
      '<td>' + window.api.formatDate(t.updated_at) + '</td>' +
      '<td>' + subAgents + '</td>' +
      '</tr>';
  }).join("");

  // 绑定点击事件：跳转到 Detail
  tbody.querySelectorAll(".thread-id-link").forEach(function(el) {
    el.addEventListener("click", function() {
      var id = el.getAttribute("data-id");
      window.loadDetail(id);
      // 激活 Detail tab
      document.querySelectorAll(".nav-tab").forEach(function(t) {
        t.classList.remove("active");
        if (t.getAttribute("data-page") === "detail") t.classList.add("active");
      });
      document.querySelectorAll(".page").forEach(function(p) { p.classList.remove("active"); });
      document.getElementById("page-detail").classList.add("active");
    });
  });
}

function renderPagination(total, page, perPage) {
  var totalPages = Math.ceil(total / perPage);
  var el = document.getElementById("threads-pagination");
  var html = "";
  html += '<button ' + (page <= 1 ? "disabled" : "") + ' data-page="' + (page-1) + '">Prev</button>';
  html += '<span style="padding:0 12px;font-size:13px;color:var(--text-muted)">' +
    page + ' / ' + totalPages + ' (' + total + ' total)</span>';
  html += '<button ' + (page >= totalPages ? "disabled" : "") + ' data-page="' + (page+1) + '">Next</button>';
  el.innerHTML = html;

  el.querySelectorAll("button").forEach(function(btn) {
    btn.addEventListener("click", function() {
      threadPage = parseInt(btn.getAttribute("data-page"));
      loadThreads();
    });
  });
}

// ── Control Events ──
document.getElementById("thread-search").addEventListener("input", function() {
  threadSearch = this.value;
  threadPage = 1;
  loadThreads();
});

document.getElementById("thread-cwd-filter").addEventListener("change", function() {
  threadCwd = this.value;
  threadPage = 1;
  loadThreads();
});

document.getElementById("thread-status-filter").addEventListener("change", function() {
  threadStatus = this.value;
  threadPage = 1;
  loadThreads();
});

document.getElementById("thread-sort").addEventListener("change", function() {
  threadSort = this.value;
  threadPage = 1;
  loadThreads();
});

document.getElementById("thread-order").addEventListener("change", function() {
  threadOrder = this.value;
  threadPage = 1;
  loadThreads();
});

document.getElementById("thread-hide-empty").addEventListener("change", function() {
  threadHideEmpty = this.checked;
  threadPage = 1;
  loadThreads();
});

})();
