(function() { "use strict";

window.originalOnPageActivate4 = window.onPageActivate;
window.onPageActivate = function(pageName) {
  if (window.originalOnPageActivate4) window.originalOnPageActivate4(pageName);
  if (pageName === "tools") { fetchToolStats(); }
};

function escHtml(s) {
  if (!s) return "";
  return String(s).replace(/&/g,"&amp;").replace(/</g,"&lt;").replace(/>/g,"&gt;").replace(/"/g,"&quot;");
}

var freqChart = null;
var errorChart = null;

async function fetchToolStats() {
  try {
    var data = await window.api.get("/api/tools/stats");
    if (data.error) return;
    renderToolFreq(data.errorRate);
    renderErrorDist(data.errorRate);
    renderErrorTable(data.recentErrors);
  } catch (e) { console.error(e); }
}

// ── 工具频次柱状图（Top 20） ──
function renderToolFreq(errorRate) {
  if (!freqChart) freqChart = window.api.initChart("chart-tool-freq", 400);
  var top = (errorRate || []).slice(0, 20);
  var names = top.map(function(t) { return t.name; });
  var counts = top.map(function(t) { return t.count; });
  window.api.updateChart(freqChart, {
    tooltip: { trigger: "axis", axisPointer: { type: "shadow" } },
    grid: { left: 140, right: 20, top: 10, bottom: 20 },
    xAxis: {
      type: "value",
      axisLine: { lineStyle: { color: "#d0d7de" } },
      axisLabel: { color: "#949aad" },
      splitLine: { lineStyle: { color: "#edf0f5" } },
    },
    yAxis: {
      type: "category", data: names,
      inverse: true,
      axisLine: { lineStyle: { color: "#d0d7de" } },
      axisLabel: { color: "#5a6072", fontSize: 11 },
    },
    series: [{
      type: "bar", data: counts,
      itemStyle: { color: "#6366f1", borderRadius: [0, 4, 4, 0] },
    }],
  });
}

// ── 错误率双轴图（Bar + Line, Top 15） ──
function renderErrorDist(errorRate) {
  if (!errorChart) errorChart = window.api.initChart("chart-tool-error", 400);
  // 按 errorCount 降序取 top 15
  var sorted = (errorRate || []).slice().sort(function(a, b) { return b.errorCount - a.errorCount; });
  var top = sorted.slice(0, 15);
  var names = top.map(function(t) { return t.name; });
  var errCounts = top.map(function(t) { return t.errorCount; });
  var rates = top.map(function(t) { return t.errorRate; });

  window.api.updateChart(errorChart, {
    tooltip: { trigger: "axis", axisPointer: { type: "shadow" } },
    legend: { data: ["Errors", "Error Rate %"], bottom: 0, textStyle: { color: "#5a6072", fontSize: 11 } },
    grid: { left: 140, right: 60, top: 20, bottom: 40 },
    xAxis: {
      type: "value",
      name: "Error Count",
      nameTextStyle: { color: "#949aad" },
      axisLine: { lineStyle: { color: "#d0d7de" } },
      axisLabel: { color: "#949aad" },
      splitLine: { lineStyle: { color: "#edf0f5" } },
    },
    yAxis: [
      {
        type: "category", data: names, inverse: true,
        axisLine: { lineStyle: { color: "#d0d7de" } },
        axisLabel: { color: "#5a6072", fontSize: 11 },
      },
      {
        type: "value", name: "%",
        nameTextStyle: { color: "#949aad" },
        axisLabel: { color: "#949aad", formatter: "{value}%" },
        splitLine: { show: false },
      },
    ],
    series: [
      {
        name: "Errors", type: "bar", data: errCounts,
        itemStyle: { color: "#ef4444", borderRadius: [0, 4, 4, 0] },
      },
      {
        name: "Error Rate %", type: "line", yAxisIndex: 1, data: rates,
        lineStyle: { color: "#f59e0b", width: 2 },
        symbol: "circle", symbolSize: 6,
        itemStyle: { color: "#f59e0b" },
      },
    ],
  });
}

// ── 最近错误表 ──
function renderErrorTable(errors) {
  var tbody = document.getElementById("errors-tbody");
  if (!errors || errors.length === 0) {
    tbody.innerHTML = '<tr><td colspan="3" class="empty-state">No errors</td></tr>';
    return;
  }

  tbody.innerHTML = errors.map(function(r) {
    var contentPreview = "";
    try {
      var parsed = JSON.parse(r.content);
      if (parsed.content) {
        contentPreview = typeof parsed.content === "string" ? parsed.content : JSON.stringify(parsed.content);
      } else {
        contentPreview = r.content;
      }
    } catch (e) { contentPreview = r.content; }

    return '<tr>' +
      '<td style="white-space:nowrap;font-size:12px;color:var(--text-muted);">Row ' + (r.msg_rowid || "-") + '</td>' +
      '<td><span class="thread-id-link" data-id="' + escHtml(r.thread_id) + '">' +
        escHtml((r.thread_title || r.thread_id).substring(0, 30)) + '</span></td>' +
      '<td><span class="error-content-preview" title="' + escHtml(contentPreview.substring(0, 500)) + '">' +
        escHtml(window.api.truncate(contentPreview, 120)) + '</span></td>' +
      '</tr>';
  }).join("");

  tbody.querySelectorAll(".thread-id-link").forEach(function(link) {
    link.addEventListener("click", function() {
      window.loadDetail(link.getAttribute("data-id"));
      document.querySelectorAll(".nav-tab").forEach(function(t) {
        t.classList.remove("active");
        if (t.getAttribute("data-page") === "detail") t.classList.add("active");
      });
      document.querySelectorAll(".page").forEach(function(p) { p.classList.remove("active"); });
      document.getElementById("page-detail").classList.add("active");
    });
  });
}

})();
