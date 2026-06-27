// ========== SCM 面板组件 ==========

(function() {

var scmList = document.getElementById("scm-list");
var scmMessage = document.getElementById("scm-message");
var scmCommitBtn = document.getElementById("scm-commit-btn");
var scmRefresh = document.getElementById("scm-refresh");

var scmData = { staged: [], unstaged: [] };
var currentDiffFile = null;
var currentTab = "changes";

// 视图模式: tree | flat
var viewMode = getViewMode();
var treeExpandState = {};

loadTreeExpandState();

function getViewMode() {
  try { return localStorage.getItem("scm:view-mode") || "flat"; } catch (e) { return "flat"; }
}
function setViewMode(mode) {
  viewMode = mode;
  try { localStorage.setItem("scm:view-mode", mode); } catch (e) {}
}
function loadTreeExpandState() {
  try {
    var saved = localStorage.getItem("scm:tree-expand");
    treeExpandState = saved ? JSON.parse(saved) : {};
  } catch (e) { treeExpandState = {}; }
}
function saveTreeExpandState() {
  try { localStorage.setItem("scm:tree-expand", JSON.stringify(treeExpandState)); } catch (e) {}
}
function updateToggleButton() {
  var btn = document.getElementById("scm-view-toggle");
  if (btn) {
    btn.textContent = viewMode === "tree" ? "▤" : "☰";
    btn.title = viewMode === "tree" ? "切换到列表视图" : "切换到树视图";
  }
}

// ---------- 状态标记映射 ----------
function statusLabelFromEntry(entry) {
  var index = entry.index;
  var worktree = entry.worktree;
  if (index === "?" && worktree === "?") return { text: "U", cls: "untracked" };
  if (index === "A") return { text: "A", cls: "added" };
  if (index === "D") return { text: "D", cls: "deleted" };
  if (index === "R") return { text: "R", cls: "renamed" };
  if (index === "M") return { text: "M", cls: "modified" };
  if (worktree === "M") return { text: "M", cls: "modified" };
  if (worktree === "D") return { text: "D", cls: "deleted" };
  return { text: "M", cls: "modified" };
}

// ---------- 构建文件树 ----------
function buildFileTree(files) {
  var tree = {};
  for (var i = 0; i < files.length; i++) {
    var parts = files[i].path.split("/");
    var node = tree;
    for (var j = 0; j < parts.length - 1; j++) {
      var dir = parts[j];
      if (!node[dir]) { node[dir] = { children: {} }; }
      node = node[dir].children;
    }
    var fileName = parts[parts.length - 1];
    node[fileName] = { entry: files[i] };
  }
  calcTreeCounts(tree);
  return tree;
}

function calcTreeCounts(tree) {
  var keys = Object.keys(tree);
  var total = 0;
  for (var i = 0; i < keys.length; i++) {
    var node = tree[keys[i]];
    if (node.children) {
      node.count = calcTreeCounts(node.children);
    } else {
      node.count = 1;
    }
    total += node.count;
  }
  return total;
}

function getSortedKeys(tree) {
  var keys = Object.keys(tree);
  keys.sort(function(a, b) {
    var aIsDir = tree[a].children !== undefined;
    var bIsDir = tree[b].children !== undefined;
    if (aIsDir && !bIsDir) return -1;
    if (!aIsDir && bIsDir) return 1;
    return a.localeCompare(b);
  });
  return keys;
}

// ---------- 渲染 SCM 文件列表 ----------
function renderScmPanel(data) {
  scmData = data;
  scmList.innerHTML = "";

  var hasStaged = data.staged && data.staged.length > 0;
  var hasUnstaged = data.unstaged && data.unstaged.length > 0;

  if (!hasStaged && !hasUnstaged) {
    showEmptyState();
    return;
  }

  if (viewMode === "tree") {
    if (hasStaged) {
      var stagedTree = buildFileTree(data.staged);
      renderTreeSection("暂存的更改", stagedTree, data.staged, true);
    }
    if (hasUnstaged) {
      var unstagedTree = buildFileTree(data.unstaged);
      renderTreeSection("更改", unstagedTree, data.unstaged, false);
    }
  } else {
    if (hasStaged) {
      renderSection("暂存的更改", data.staged, true);
    }
    if (hasUnstaged) {
      renderSection("更改", data.unstaged, false);
    }
  }
}

function showEmptyState() {
  scmList.innerHTML = '<div id="scm-empty">' +
    '<div class="scm-empty-icon">✓</div>' +
    '<div>没有更改</div>' +
    '</div>';
}

// ---------- 渲染一个分区 ----------
function renderSection(title, files, isStaged) {
  var section = document.createElement("div");
  section.className = "scm-section";

  var header = document.createElement("div");
  header.className = "scm-section-header";
  header.innerHTML = '<span>' + title + ' (' + files.length + ')</span>';

  var actions = document.createElement("div");
  actions.className = "scm-section-actions";

  if (isStaged) {
    var unstageAllBtn = document.createElement("button");
    unstageAllBtn.title = "全部取消暂存";
    unstageAllBtn.textContent = "−";
    unstageAllBtn.addEventListener("click", function(e) {
      e.stopPropagation();
      stageFiles(files.map(function(f) { return f.path; }), false);
    });
    actions.appendChild(unstageAllBtn);
  } else {
    var stageAllBtn = document.createElement("button");
    stageAllBtn.title = "全部暂存";
    stageAllBtn.textContent = "+";
    stageAllBtn.addEventListener("click", function(e) {
      e.stopPropagation();
      stageFiles(files.map(function(f) { return f.path; }), true);
    });
    actions.appendChild(stageAllBtn);
  }

  header.appendChild(actions);
  section.appendChild(header);

  for (var i = 0; i < files.length; i++) {
    var fileRow = renderFileRow(files[i], isStaged);
    section.appendChild(fileRow);
  }

  scmList.appendChild(section);
}

// ---------- 渲染单文件行 ----------
function renderFileRow(entry, isStaged, depth, hidePath) {
  var row = document.createElement("div");
  row.className = "scm-file";
  row.dataset.path = entry.path;
  row.dataset.staged = isStaged ? "true" : "false";

  // 树模式缩进
  if (depth) {
    row.style.paddingLeft = (depth * 16 + 12) + "px";
  }

  var sl = statusLabelFromEntry(entry);

  var actionBtn = document.createElement("button");
  actionBtn.className = "scm-file-action";
  actionBtn.title = isStaged ? "取消暂存" : "暂存";
  actionBtn.textContent = isStaged ? "−" : "+";
  actionBtn.addEventListener("click", function(e) {
    e.stopPropagation();
    stageFiles([entry.path], !isStaged);
  });
  row.appendChild(actionBtn);

  var statusEl = document.createElement("span");
  statusEl.className = "scm-status-badge " + sl.cls;
  statusEl.textContent = sl.text;
  row.appendChild(statusEl);

  var nameParts = entry.path.split("/");
  var nameEl = document.createElement("span");
  nameEl.className = "scm-file-name";
  nameEl.textContent = nameParts[nameParts.length - 1];
  row.appendChild(nameEl);

  var pathEl = document.createElement("span");
  pathEl.className = "scm-file-path";
  if (!hidePath) {
    var dirPath = nameParts.slice(0, -1).join("/");
    if (dirPath) {
      pathEl.textContent = dirPath;
    }
  }
  row.appendChild(pathEl);

  if (!isStaged && entry.worktree !== "?") {
    var discardBtn = document.createElement("button");
    discardBtn.className = "scm-file-action";
    discardBtn.title = "放弃更改";
    discardBtn.textContent = "↩";
    discardBtn.addEventListener("click", function(e) {
      e.stopPropagation();
      discardFile(entry.path);
    });
    row.appendChild(discardBtn);
  }

  row.addEventListener("click", function() {
    viewDiff(entry.path, isStaged, nameParts[nameParts.length - 1]);
  });

  return row;
}

// ---------- 渲染树节点 ----------
function renderTreeSection(title, fileTree, sectionFiles, isStaged) {
  var section = document.createElement("div");
  section.className = "scm-section";

  var header = document.createElement("div");
  header.className = "scm-section-header";
  header.innerHTML = '<span>' + title + ' (' + sectionFiles.length + ')</span>';

  var actions = document.createElement("div");
  actions.className = "scm-section-actions";

  if (isStaged) {
    var unstageAllBtn = document.createElement("button");
    unstageAllBtn.title = "全部取消暂存";
    unstageAllBtn.textContent = "−";
    unstageAllBtn.addEventListener("click", function(e) {
      e.stopPropagation();
      stageFiles(sectionFiles.map(function(f) { return f.path; }), false);
    });
    actions.appendChild(unstageAllBtn);
  } else {
    var stageAllBtn = document.createElement("button");
    stageAllBtn.title = "全部暂存";
    stageAllBtn.textContent = "+";
    stageAllBtn.addEventListener("click", function(e) {
      e.stopPropagation();
      stageFiles(sectionFiles.map(function(f) { return f.path; }), true);
    });
    actions.appendChild(stageAllBtn);
  }

  header.appendChild(actions);
  section.appendChild(header);
  renderTreeChildren(fileTree, section, 0, isStaged, "");
  scmList.appendChild(section);
}

function renderTreeChildren(tree, container, depth, isStaged, parentPath) {
  var keys = getSortedKeys(tree);
  for (var i = 0; i < keys.length; i++) {
    var key = keys[i];
    var node = tree[key];
    renderTreeNode(key, node, container, depth, isStaged, parentPath);
  }
}

function renderTreeNode(key, node, container, depth, isStaged, parentPath) {
  var fullPath = parentPath ? parentPath + "/" + key : key;

  if (node.children) {
    // 文件夹节点
    var folderDiv = document.createElement("div");
    folderDiv.className = "scm-tree-folder";
    folderDiv.style.paddingLeft = (depth * 16 + 12) + "px";
    folderDiv.dataset.folderPath = fullPath;

    var arrow = document.createElement("span");
    arrow.className = "scm-tree-arrow";
    arrow.textContent = "▶";

    var nameEl = document.createElement("span");
    nameEl.className = "scm-tree-folder-name";
    nameEl.textContent = key;

    var countEl = document.createElement("span");
    countEl.className = "scm-tree-count";
    countEl.textContent = "(" + node.count + ")";

    folderDiv.appendChild(arrow);
    folderDiv.appendChild(nameEl);
    folderDiv.appendChild(countEl);

    var childrenDiv = document.createElement("div");
    childrenDiv.className = "scm-tree-children collapsed";

    if (treeExpandState[fullPath]) {
      childrenDiv.classList.remove("collapsed");
      arrow.classList.add("expanded");
    }

    renderTreeChildren(node.children, childrenDiv, depth + 1, isStaged, fullPath);

    folderDiv.addEventListener("click", function(e) {
      e.stopPropagation();
      var isExpanded = !childrenDiv.classList.contains("collapsed");
      if (isExpanded) {
        childrenDiv.classList.add("collapsed");
        arrow.classList.remove("expanded");
        delete treeExpandState[fullPath];
      } else {
        childrenDiv.classList.remove("collapsed");
        arrow.classList.add("expanded");
        treeExpandState[fullPath] = true;
      }
      saveTreeExpandState();
    });

    container.appendChild(folderDiv);
    container.appendChild(childrenDiv);
  } else {
    // 文件叶子节点
    var fileRow = renderFileRow(node.entry, isStaged, depth, true);
    container.appendChild(fileRow);
  }
}

// ---------- 视图切换 ----------
function toggleViewMode() {
  var newMode = viewMode === "flat" ? "tree" : "flat";
  setViewMode(newMode);
  if (newMode === "tree") {
    treeExpandState = {};
    saveTreeExpandState();
  }
  refreshScm();
  updateToggleButton();
}

// ---------- stage / unstage ----------
async function stageFiles(files, toStage) {
  try {
    var resp = await fetch("/api/scm/stage", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ files: files, staged: toStage }),
    });
    var data = await resp.json();
    if (data.success) {
      await refreshScm();
    } else {
      alert("操作失败: " + (data.error || "未知错误"));
    }
  } catch (err) {
    alert("请求失败: " + (err.message || String(err)));
  }
}

// ---------- discard ----------
async function discardFile(filePath) {
  if (!confirm("确定要放弃对 '" + filePath + "' 的更改吗？此操作不可撤销。")) return;
  try {
    var resp = await fetch("/api/scm/discard", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ files: [filePath] }),
    });
    var data = await resp.json();
    if (data.success) {
      await refreshScm();
    } else {
      alert("操作失败: " + (data.error || "未知错误"));
    }
  } catch (err) {
    alert("请求失败: " + (err.message || String(err)));
  }
}

// ---------- 查看 diff ----------
async function viewDiff(filePath, staged, name) {
  currentDiffFile = { path: filePath, staged: staged, name: name };
  try {
    var resp = await fetch("/api/scm/diff?file=" + encodeURIComponent(filePath) + "&staged=" + (staged ? "true" : "false"));
    var data = await resp.json();
    if (data.diff !== undefined) {
      renderDiffView(filePath, name, data.diff);
    }
  } catch (err) {
    console.error("获取 diff 失败:", err);
  }
}

// ---------- 手动渲染 diff（fallback，不依赖 diff2html）----------
function renderDiffFallback(diffText) {
  var lines = diffText.split("\n");
  var html = '<div class="diff-fallback">';
  for (var i = 0; i < lines.length; i++) {
    var line = escapeHtml(lines[i]);
    var cls = "";
    if (line.indexOf("+") === 0 && line.indexOf("+++") !== 0) cls = "diff-add";
    else if (line.indexOf("-") === 0 && line.indexOf("---") !== 0) cls = "diff-del";
    else if (line.indexOf("@@") === 0) cls = "diff-hunk";
    else if (line.indexOf("diff ") === 0 || line.indexOf("index ") === 0 ||
             line.indexOf("--- ") === 0 || line.indexOf("+++ ") === 0 ||
             line.indexOf("new file") === 0 || line.indexOf("deleted file") === 0 ||
             line.indexOf("rename ") === 0 || line.indexOf("similarity ") === 0) cls = "diff-meta";
    html += '<div class="' + cls + '">' + (line || "&nbsp;") + '</div>';
  }
  html += '</div>';
  return html;
}

// ---------- 渲染 diff ----------
function renderDiffView(path, name, diffText) {
  var preview = document.getElementById("preview");
  var diffHtml = "";

  if (!diffText) {
    diffHtml = '<div style="padding:40px;color:var(--text-muted);text-align:center;">没有差异</div>';
  } else if (typeof Diff2Html !== "undefined" && Diff2Html.html) {
    try {
      diffHtml = Diff2Html.html(diffText, {
        drawFileList: false,
        matching: "lines",
        outputFormat: "line-by-line",
      });
    } catch (e) {
      console.error("Diff2Html 渲染失败，使用 fallback:", e);
      diffHtml = renderDiffFallback(diffText);
    }
  } else {
    console.warn("Diff2Html 未加载，使用 fallback 渲染");
    diffHtml = renderDiffFallback(diffText);
  }

  var scheme = (document.documentElement.dataset.theme || "dark") === "dark" ? " d2h-dark-color-scheme" : "";
  preview.innerHTML =
    '<div class="diff-view' + scheme + '">' +
    '<div class="file-info-bar">' +
    '<span class="file-name">' + escapeHtml(name) + ' (diff)</span>' +
    '<span class="file-size" style="color:var(--text-muted);">变更对比</span>' +
    '</div>' +
    diffHtml +
    '</div>';
}

// ---------- 提交 ----------
async function doCommit() {
  var message = scmMessage.value.trim();
  if (!message) return;

  scmCommitBtn.disabled = true;
  scmCommitBtn.textContent = "提交中...";

  try {
    var resp = await fetch("/api/scm/commit", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ message: message }),
    });
    var data = await resp.json();
    if (data.success) {
      scmMessage.value = "";
      await refreshScm();
    } else {
      alert("提交失败: " + (data.error || "未知错误"));
    }
  } catch (err) {
    alert("请求失败: " + (err.message || String(err)));
  } finally {
    scmCommitBtn.disabled = false;
    scmCommitBtn.textContent = "提交";
  }
}

// ---------- 刷新 SCM 面板 ----------
async function refreshScm() {
  try {
    var resp = await fetch("/api/scm/status");
    var data = await resp.json();

    if (!data.hasRepo) {
      scmList.innerHTML = '<div class="scm-no-git"><div>当前目录不是 Git 仓库</div></div>';
      return;
    }

    if (data.error) {
      scmList.innerHTML = '<div class="scm-no-git"><div>获取状态失败: ' + escapeHtml(data.error) + '</div></div>';
      return;
    }

    renderScmPanel(data);

    window.dispatchEvent(new CustomEvent("scm-changed"));

  } catch (err) {
    scmList.innerHTML = '<div class="scm-no-git"><div>无法连接服务器</div></div>';
  }
}

// ---------- Tab 切换 ----------
function initScmResizer(showCommit) {
  var commitArea = document.getElementById("scm-commit-area");
  if (commitArea) {
    commitArea.style.display = showCommit ? "" : "none";
  }
}

function switchTab(tab) {
  var tabChanges = document.getElementById("scm-tab-changes");
  var tabGraph = document.getElementById("scm-tab-graph");
  var overlay = document.getElementById("graph-overlay");

  if (tab === "graph") {
    currentTab = "graph";
    // 显示浮动面板
    if (overlay) overlay.style.display = "";
    if (tabGraph) tabGraph.classList.add("active");
    if (tabChanges) tabChanges.classList.remove("active");

    // Graph 模式下隐藏 SCM 提交区域
    initScmResizer(false);

    setTimeout(function() {
      if (window.refreshGraph) window.refreshGraph();
    }, 50);
  } else {
    // 隐藏浮动面板
    if (overlay) overlay.style.display = "none";
    currentTab = "changes";
    if (tabChanges) tabChanges.classList.add("active");
    if (tabGraph) tabGraph.classList.remove("active");

    initScmResizer(true);
    // 切回 Changes 时刷新
    refreshScm();
  }
}

function openGraphOverlay() {
  switchTab("graph");
}

function closeGraphOverlay() {
  switchTab("changes");
}

// ---------- 初始化 ----------
function initScm() {
  scmRefresh.addEventListener("click", function() {
    if (currentTab === "graph") {
      if (window.refreshGraph) window.refreshGraph();
    } else {
      refreshScm();
    }
  });

  // Tab 按钮
  var tabChanges = document.getElementById("scm-tab-changes");
  var tabGraph = document.getElementById("scm-tab-graph");
  if (tabChanges) tabChanges.addEventListener("click", function() { switchTab("changes"); });
  if (tabGraph) tabGraph.addEventListener("click", function() { switchTab("graph"); });

  // Overlay 关闭
  var overlayClose = document.getElementById("graph-overlay-close");
  var overlayBackdrop = document.querySelector(".graph-overlay-backdrop");
  if (overlayClose) overlayClose.addEventListener("click", closeGraphOverlay);
  if (overlayBackdrop) overlayBackdrop.addEventListener("click", closeGraphOverlay);

  // Overlay 刷新
  var overlayRefresh = document.getElementById("graph-overlay-refresh");
  if (overlayRefresh) overlayRefresh.addEventListener("click", function() {
    if (window.refreshGraph) window.refreshGraph();
  });

  // Esc 关闭 overlay
  document.addEventListener("keydown", function(e) {
    if (e.key === "Escape") {
      var overlay = document.getElementById("graph-overlay");
      if (overlay && overlay.style.display !== "none") {
        closeGraphOverlay();
      }
    }
  });

  // 视图切换按钮
  var toggleBtn = document.getElementById("scm-view-toggle");
  if (toggleBtn) {
    toggleBtn.addEventListener("click", toggleViewMode);
    updateToggleButton();
  }

  scmCommitBtn.addEventListener("click", doCommit);

  scmMessage.addEventListener("input", function() {
    scmCommitBtn.disabled = !scmMessage.value.trim();
  });

  scmMessage.addEventListener("keydown", function(e) {
    if ((e.ctrlKey || e.metaKey) && e.key === "Enter") {
      e.preventDefault();
      if (scmMessage.value.trim()) {
        doCommit();
      }
    }
  });

  setTimeout(function() {
    refreshScm();
  }, 500);
}

// 暴露到全局
window.refreshScm = refreshScm;

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", initScm);
} else {
  initScm();
}

})();
