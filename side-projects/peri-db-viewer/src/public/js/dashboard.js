(function() { "use strict";

// 保存前一个 handler
window.originalOnPageActivate1 = window.onPageActivate;
window.onPageActivate = function(pageName) {
  if (window.originalOnPageActivate1) window.originalOnPageActivate1(pageName);
  if (pageName === "dashboard") { fetchDashboard(); }
};

async function fetchDashboard() {
  try {
    var data = await window.api.get("/api/stats");
    if (data.error) return;
    renderStatsCards(data);
    renderRolePie(data.roleDistribution);
    renderAgentStatusRing(data.agentStatusDist);
  } catch (e) { console.error(e); }

  try {
    var timeline = await window.api.get("/api/timeline?days=30");
    if (!timeline.error) renderTimeline(timeline);
  } catch (e) { console.error(e); }

  try {
    var toolData = await window.api.get("/api/tools/stats");
    if (!toolData.error) renderTopTools(toolData.errorRate);
  } catch (e) { console.error(e); }
}

// ── Stats Cards ──
function renderStatsCards(data) {
  var el = document.getElementById("stats-cards");
  var cards = [
    { label: "Total Sessions", value: data.totalThreads },
    { label: "Visible Sessions", value: data.visibleThreads },
    { label: "Total Messages", value: data.totalMessages },
    { label: "Sub-Agents", value: data.totalSubAgents },
    { label: "Tool Errors", value: data.totalToolErrors },
    { label: "Status Types", value: (data.agentStatusDist || []).length },
  ];
  el.innerHTML = cards.map(function(c) {
    return '<div class="stat-card"><div class="stat-label">' + c.label +
      '</div><div class="stat-value">' + c.value + '</div></div>';
  }).join("");
}

// ── Role Pie Chart ──
var rolePieChart = null;
var topToolsChart = null;
var timelineChart = null;
var statusRingChart = null;

function renderRolePie(roleDist) {
  if (!rolePieChart) rolePieChart = window.api.initChart("chart-role-pie", 300);
  var roles = Object.keys(roleDist || {});
  var data = roles.map(function(r) { return { name: r, value: roleDist[r] }; });
  window.api.updateChart(rolePieChart, {
    tooltip: { trigger: "item" },
    legend: { bottom: 0, textStyle: { color: "#5a6072", fontSize: 11 } },
    series: [{
      type: "pie",
      radius: ["42%", "72%"],
      center: ["50%", "45%"],
      itemStyle: { borderRadius: 4, borderColor: "#ffffff", borderWidth: 3 },
      label: { color: "#5a6072", fontSize: 12 },
      data: data,
    }],
  });
}

// ── Timeline Line Chart ──
function renderTimeline(data) {
  if (!timelineChart) timelineChart = window.api.initChart("chart-timeline", 300);
  var dates = data.map(function(d) { return d.date; });
  var counts = data.map(function(d) { return d.count; });
  window.api.updateChart(timelineChart, {
    tooltip: { trigger: "axis" },
    grid: { left: 50, right: 20, top: 20, bottom: 30 },
    xAxis: {
      type: "category", data: dates,
      axisLine: { lineStyle: { color: "#d0d7de" } },
      axisLabel: { color: "#949aad", fontSize: 11, rotate: 45 },
    },
    yAxis: {
      type: "value",
      axisLine: { lineStyle: { color: "#d0d7de" } },
      axisLabel: { color: "#949aad" },
      splitLine: { lineStyle: { color: "#edf0f5" } },
    },
    series: [{
      type: "line", data: counts,
      smooth: true,
      lineStyle: { color: "#3b82f6", width: 2 },
      areaStyle: { color: new echarts.graphic.LinearGradient(0,0,0,1,[
        { offset: 0, color: "rgba(59,130,246,0.15)" },
        { offset: 1, color: "rgba(59,130,246,0)" },
      ])},
      symbol: "none",
    }],
  });
}

// ── Top Tools Bar Chart ──
function renderTopTools(errorRate) {
  if (!topToolsChart) topToolsChart = window.api.initChart("chart-top-tools", 300);
  var top = (errorRate || []).slice(0, 15);
  var names = top.map(function(t) { return t.name; });
  var counts = top.map(function(t) { return t.count; });
  window.api.updateChart(topToolsChart, {
    tooltip: { trigger: "axis", axisPointer: { type: "shadow" } },
    grid: { left: 120, right: 20, top: 10, bottom: 20 },
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
      itemStyle: { color: "#3b82f6", borderRadius: [0, 4, 4, 0] },
    }],
  });
}

// ── Agent Status Ring Chart ──
function renderAgentStatusRing(statusDist) {
  if (!statusRingChart) statusRingChart = window.api.initChart("chart-status-ring", 300);
  var items = (statusDist || []).map(function(s) {
    return { name: s.agent_status, value: s.count };
  });
  window.api.updateChart(statusRingChart, {
    tooltip: { trigger: "item" },
    legend: { bottom: 0, textStyle: { color: "#5a6072", fontSize: 11 } },
    series: [{
      type: "pie",
      radius: ["45%", "75%"],
      center: ["50%", "45%"],
      roseType: "area",
      itemStyle: { borderRadius: 4, borderColor: "#ffffff", borderWidth: 3 },
      label: { color: "#5a6072", fontSize: 12 },
      data: items,
    }],
  });
}

})();
