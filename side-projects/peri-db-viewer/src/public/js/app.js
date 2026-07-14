(function() { "use strict";

// ── 全局 API 工具 ──
window.api = {
  get: async function(url) {
    const resp = await fetch(url);
    return resp.json();
  },

  formatDate: function(iso) {
    if (!iso) return "-";
    const d = new Date(iso);
    return d.toLocaleDateString("zh-CN") + " " + d.toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" });
  },

  truncate: function(str, len) {
    if (!str) return "";
    return str.length > len ? str.substring(0, len) + "..." : str;
  },

  initChart: function(domId, height) {
    const el = document.getElementById(domId);
    if (!el) return null;
    if (height) el.style.height = height + "px";
    const chart = echarts.init(el, null, { renderer: "canvas" });
    // resize on window resize
    const observer = new ResizeObserver(function() { chart.resize(); });
    observer.observe(el);
    return chart;
  },

  updateChart: function(chart, option) {
    if (!chart) return;
    chart.setOption(option, true);
  }
};

// ── 页面激活链条 ──
window.onPageActivate = function(pageName) {
  // 默认：切换页面可见性
  document.querySelectorAll(".page").forEach(function(p) {
    p.classList.remove("active");
  });
  const target = document.getElementById("page-" + pageName);
  if (target) target.classList.add("active");
};

// ── Tab 切换 ──
document.addEventListener("DOMContentLoaded", function() {
  var tabs = document.querySelectorAll(".nav-tab");
  tabs.forEach(function(tab) {
    tab.addEventListener("click", function() {
      tabs.forEach(function(t) { t.classList.remove("active"); });
      tab.classList.add("active");
      var page = tab.getAttribute("data-page");
      if (window.onPageActivate) {
        window.onPageActivate(page);
      }
    });
  });
});

})();
