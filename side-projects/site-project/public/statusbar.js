// ========== 底部状态栏组件 ==========

(function() {

var branchName = document.getElementById("branch-name");
var gitAhead = document.getElementById("git-ahead");
var gitBehind = document.getElementById("git-behind");
var gitStatsText = document.getElementById("git-stats-text");
var statusbarFile = document.getElementById("statusbar-file");
var branchDropdown = document.getElementById("branch-dropdown");
var branchList = document.getElementById("branch-list");
var statusbarBranch = document.getElementById("statusbar-branch");

var currentBranch = "";

// ---------- 更新分支信息 ----------
function updateBranch(name, ahead, behind) {
  currentBranch = name || "";
  window._currentBranch = currentBranch;  // 暴露给 graph.js 使用
  branchName.textContent = name || "";

  if (ahead && ahead > 0) {
    gitAhead.textContent = ahead;
    gitAhead.style.display = "";
  } else {
    gitAhead.textContent = "";
    gitAhead.style.display = "none";
  }

  if (behind && behind > 0) {
    gitBehind.textContent = behind;
    gitBehind.style.display = "";
  } else {
    gitBehind.textContent = "";
    gitBehind.style.display = "none";
  }
}

// ---------- 更新变更统计 ----------
function updateStats(added, modified, deleted) {
  var parts = [];
  if (added > 0) parts.push("+" + added);
  if (modified > 0) parts.push("~" + modified);
  if (deleted > 0) parts.push("-" + deleted);
  gitStatsText.textContent = parts.length > 0 ? "  " + parts.join(" ") : "";
}

// ---------- 更新当前文件信息 ----------
function updateFileInfo(name) {
  statusbarFile.textContent = name || "";
}

// ---------- 显示分支下拉 ----------
async function showBranchDropdown() {
  try {
    var resp = await fetch("/api/scm/branches");
    var data = await resp.json();
    if (data.error) return;

    branchList.innerHTML = "";
    for (var i = 0; i < data.branches.length; i++) {
      var b = data.branches[i];
      var item = document.createElement("div");
      item.className = "dropdown-item" + (b.current ? " current" : "") + (b.remote ? " remote" : "");
      item.innerHTML = '<span class="check-mark">' + (b.current ? "✓" : "") + '</span>' +
        '<span>' + escapeHtml(b.name) + '</span>';
              item.dataset.branch = b.name;
        item.dataset.remote = b.remote ? "1" : "";
      item.addEventListener("click", function() {
        var name = this.dataset.branch;
        switchBranch(name);
        hideBranchDropdown();
      });
      branchList.appendChild(item);
    }

    branchDropdown.classList.remove("dropdown-hidden");
  } catch (err) {
    // 静默失败
  }
}

function hideBranchDropdown() {
  branchDropdown.classList.add("dropdown-hidden");
}

// ---------- 切换分支 ----------
async function switchBranch(name) {
  try {
    var resp = await fetch("/api/scm/branch", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ branch: name }),
    });
    var data = await resp.json();
    if (data.success) {
      if (window.refreshScm) window.refreshScm();
      refreshStatusBar();
    } else {
      alert("切换分支失败: " + (data.error || "未知错误"));
    }
  } catch (err) {
    alert("请求失败: " + (err.message || String(err)));
  }
}

// ---------- Checkout 远程分支 ----------
async function switchRemoteBranch(name) {
  if (!confirm("Checkout remote branch '" + name + "' as local tracking branch?")) return;
  try {
    var resp = await fetch("/api/scm/checkout-remote", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ branch: name }),
    });
    var data = await resp.json();
    if (data.success) {
      if (window.refreshScm) window.refreshScm();
      refreshStatusBar();
    } else {
      alert("Checkout 失败: " + (data.error || "未知错误"));
    }
  } catch (err) {
    alert("请求失败: " + (err.message || String(err)));
  }
}

// ---------- 刷新状态栏 ----------
async function refreshStatusBar() {
  try {
    var resp = await fetch("/api/scm/summary");
    var data = await resp.json();
    if (!data.hasRepo) return;

    updateBranch(data.branch, data.ahead, data.behind);
    updateStats(data.added, data.modified, data.deleted);
  } catch (err) {
    // 静默失败
  }
}

// ---------- 事件监听 ----------
statusbarBranch.addEventListener("click", function(e) {
  e.stopPropagation();
  if (branchDropdown.classList.contains("dropdown-hidden")) {
    showBranchDropdown();
  } else {
    hideBranchDropdown();
  }
});

document.addEventListener("click", function(e) {
  if (!branchDropdown.classList.contains("dropdown-hidden")) {
    if (!branchDropdown.contains(e.target) && e.target !== statusbarBranch) {
      hideBranchDropdown();
    }
  }
});

window.addEventListener("scm-changed", function() {
  refreshStatusBar();
});

window.addEventListener("file-select", function(e) {
  updateFileInfo(e.detail.path);
});

// ---------- 初始化 ----------
function initStatusBar() {
  refreshStatusBar();
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", initStatusBar);
} else {
  initStatusBar();
}

window.refreshStatusBar = refreshStatusBar;

})();
