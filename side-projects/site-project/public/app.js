// ========== 主应用入口 ==========

(function() {
  var treeContainer = document.getElementById("file-tree");
  var collapseBtn = document.getElementById("collapse-all");
  var clearCacheBtn = document.getElementById("clear-cache");
  var resizer = document.getElementById("sidebar-resizer");
  var sidebar = document.getElementById("sidebar");
  var scmResizer = document.getElementById("scm-resizer");
  var fileTree = document.getElementById("file-tree");
  var scmPanel = document.getElementById("scm-panel");

  var isAllExpanded = true;
  var hasGitRepo = false;

  var currentFilePath = null;
  var currentFileMtime = null;
  var pollTimer = null;
  var POLL_INTERVAL = 2000;

  // ---------- Git 仓库检测 ----------
  async function detectGit() {
    try {
      var resp = await fetch("/api/scm/detect");
      var data = await resp.json();
      hasGitRepo = data.hasRepo;
      if (!hasGitRepo) {
        scmPanel.style.display = "none";
        scmResizer.style.display = "none";
        fileTree.style.flex = "1";
      }
    } catch (e) {
      hasGitRepo = false;
      scmPanel.style.display = "none";
      scmResizer.style.display = "none";
      fileTree.style.flex = "1";
    }
  }

  // ---------- 加载文件树 ----------
  async function loadTree() {
    try {
      var cached = cacheGet("tree:root");
      if (cached) {
        treeContainer.innerHTML = "";
        renderTree(treeContainer, cached);
        setTimeout(function() { restoreExpandedState(); }, 100);
        refreshTreeSilently();
        return;
      }

      treeContainer.innerHTML = '<div style="padding:20px;color:var(--text-muted);text-align:center;">' +
        '<div class="spinner" style="margin:0 auto;"></div></div>';

      var resp = await fetch("/api/tree");
      var nodes = await resp.json();

      if (nodes.error) throw new Error(nodes.error);

      cacheSet("tree:root", nodes);
      treeContainer.innerHTML = "";
      renderTree(treeContainer, nodes);
      setTimeout(function() { restoreExpandedState(); }, 100);
    } catch (err) {
      treeContainer.innerHTML = '<div style="padding:20px;color:#cf222e;">加载失败: ' +
        (err.message || String(err)) + '</div>';
    }
  }

  async function refreshTreeSilently() {
    try {
      var resp = await fetch("/api/tree");
      var nodes = await resp.json();
      if (!nodes.error) cacheSet("tree:root", nodes);
    } catch (e) {}
  }

  // ---------- 轮询文件变化 ----------
  function startPolling(path, mtime) {
    stopPolling();
    if (!path) return;

    currentFilePath = path;
    currentFileMtime = mtime;

    pollTimer = setInterval(async function() {
      if (!currentFilePath) { stopPolling(); return; }

      try {
        var resp = await fetch("/api/stat?path=" + encodeURIComponent(currentFilePath));
        var data = await resp.json();
        if (data.error) return;

        if (data.mtime && data.mtime !== currentFileMtime) {
          currentFileMtime = data.mtime;
          renderPreview(currentFilePath, currentFilePath.split("/").pop());

          if (hasGitRepo && window.refreshScm) {
            window.refreshScm();
          }
        }
      } catch (e) {
        // 静默失败
      }
    }, POLL_INTERVAL);
  }

  function stopPolling() {
    if (pollTimer) {
      clearInterval(pollTimer);
      pollTimer = null;
    }
    currentFilePath = null;
    currentFileMtime = null;
  }

  // ---------- 文件选择事件 ----------
  window.addEventListener("file-select", function(e) {
    var detail = e.detail;
    var name = detail.path.split("/").pop();

    showPreview(name);

    fetch("/api/stat?path=" + encodeURIComponent(detail.path))
      .then(function(resp) { return resp.json(); })
      .then(function(data) {
        if (!data.error) {
          currentFileMtime = data.mtime;
        }
      })
      .catch(function() {});

    startPolling(detail.path, currentFileMtime);
    renderPreview(detail.path, detail.name);
    syncWorkspace(); // 保存活动文件到工作区
  });

  // ---------- 分支切换事件 ----------
  window.addEventListener("branch-changed", function() {
    stopPolling();
    treeContainer.innerHTML = "";
    clearCacheBtn.click();  // 清缓存并重载
  });

  // ---------- 折叠/展开按钮 ----------
  collapseBtn.addEventListener("click", function() {
    isAllExpanded = !isAllExpanded;
    if (isAllExpanded) {
      expandAll();
      collapseBtn.title = "折叠全部";
    } else {
      collapseAll();
      collapseBtn.title = "展开全部";
    }
  });

  // ---------- 清除缓存按钮 ----------
  clearCacheBtn.addEventListener("click", function() {
    stopPolling();
    cacheClear();
    localStorage.removeItem(CACHE_PREFIX + "state:expanded");
    treeContainer.innerHTML = "";
    loadTree();
    if (hasGitRepo && window.refreshScm) {
      window.refreshScm();
    }
  });

  // ========== 统一拖拽系统 ==========
  var dragState = null;
  // { type:"sidebar"|"scm"|"center", startX, startY, mask, startWidth?, flexStart?, startFlexBasis? }

  function dragIframe(disable) {
    var iframe = document.querySelector("#terminal-container iframe");
    if (iframe) iframe.style.pointerEvents = disable ? "none" : "";
  }

  function startDrag(type, e, extra) {
    var st = { type: type, startX: e.clientX, startY: e.clientY };
    for (var k in extra) st[k] = extra[k];

    var mask = document.createElement("div");
    mask.style.cssText = "position:fixed;inset:0;z-index:9999;cursor:" + extra.cursor + ";";
    document.body.appendChild(mask);
    st.mask = mask;

    dragIframe(true);
    dragState = st;
    document.body.style.userSelect = "none";
  }

  function endDrag() {
    if (!dragState) return;
    dragState.mask.remove();
    dragState = null;
    document.body.style.userSelect = "";
    dragIframe(false);
    document.querySelectorAll(
      "#sidebar-resizer.active, #scm-resizer.active, #center-resizer.active"
    ).forEach(function(el) { el.classList.remove("active"); });
    syncWorkspace();
  }

  document.addEventListener("mousemove", function(e) {
    var ds = dragState;
    if (!ds) return;
    switch (ds.type) {
      case "sidebar":
        var d = e.clientX - ds.startX;
        sidebar.style.width = Math.min(500, Math.max(180, ds.startWidth + d)) + "px";
        break;
      case "scm":
        var d2 = e.clientY - ds.startY;
        var frac = d2 / Math.max(sidebar.offsetHeight, 1);
        fileTree.style.flex = Math.min(8, Math.max(1, ds.flexStart + frac * 5)) + " 1 0";
        break;
      case "center":
        if (!previewWrapper || previewWrapper.classList.contains("preview-hidden")) break;
        var d3 = e.clientX - ds.startX;
        var totalW = centerResizer.parentElement.getBoundingClientRect().width;
        var newBasis = ds.startFlexBasis + d3;
        _terminalFlex = Math.max(0.5, Math.min(5, newBasis / (totalW - 4) * 3));
        terminalPanel.style.flex = _terminalFlex + " 1 0";
        break;
    }
  });

  document.addEventListener("mouseup", endDrag);

  // 绑定各 resizer
  resizer.addEventListener("mousedown", function(e) {
    resizer.classList.add("active");
    startDrag("sidebar", e, { cursor: "col-resize", startWidth: sidebar.offsetWidth });
  });

  scmResizer.addEventListener("mousedown", function(e) {
    scmResizer.classList.add("active");
    startDrag("scm", e, { cursor: "row-resize", flexStart: parseInt(getComputedStyle(fileTree).flexGrow) || 3 });
  });

  // ---------- 页面卸载时停止轮询 ----------
  window.addEventListener("beforeunload", function() {
    stopPolling();
  });

  // ---------- Getman 面板触发 ----------
  var getmanBtn = document.getElementById("statusbar-getman");
  if (getmanBtn) {
    getmanBtn.addEventListener("click", function() {
      if (window._getmanOpen) window._getmanOpen();
    });
  }

  document.addEventListener("keydown", function(e) {
    if ((e.ctrlKey || e.metaKey) && e.key === "k") {
      e.preventDefault();
      if (window._getmanOpen) window._getmanOpen();
    }
  });

  // ---------- Terminal: 始终嵌入 ----------
  (function embedTerminal() {
    var container = document.getElementById("terminal-container");
    if (!container) return;
    var iframe = document.createElement("iframe");
    iframe.src = "/terminal.html";
    iframe.style.cssText = "width:100%;height:100%;border:none;";
    container.appendChild(iframe);
  })();

  // ---------- 预览面板显示/隐藏 ----------
  var previewWrapper = document.getElementById("preview-wrapper");
  var previewFilename = document.getElementById("preview-filename");
  var previewCloseBtn = document.getElementById("preview-close");
  var _terminalFlex = 2; // 保存的终端 flex 值

  function showPreview(name) {
    if (previewWrapper) previewWrapper.classList.remove("preview-hidden");
    if (previewFilename) previewFilename.textContent = name;
    // 预览从 display:none 恢复后，重新应用保存的终端 flex 比例
    if (terminalPanel) terminalPanel.style.flex = _terminalFlex + " 1 0";
  }
  function hidePreview() {
    if (previewWrapper) previewWrapper.classList.add("preview-hidden");
    stopPolling();
    currentFilePath = null;
    syncWorkspace();
  }

  if (previewCloseBtn) {
    previewCloseBtn.addEventListener("click", hidePreview);
  }

  // ---------- 终端/预览分割线拖拽（统一拖拽系统接管）----------
  var centerResizer = document.getElementById("center-resizer");
  var terminalPanel = document.getElementById("terminal-panel");

  centerResizer.addEventListener("mousedown", function(e) {
    if (!previewWrapper || previewWrapper.classList.contains("preview-hidden")) return;
    centerResizer.classList.add("active");
    startDrag("center", e, { cursor: "col-resize", startFlexBasis: terminalPanel.getBoundingClientRect().width });
  });

  // ---------- 工作区同步 ----------
  var _syncTimer = null;
  var _workspaceLoaded = false;

  async function syncWorkspace() {
    if (!_workspaceLoaded) return;
    clearTimeout(_syncTimer);
    _syncTimer = setTimeout(async function() {
      try {
        var payload = {
          fileTree: {
            expandedDirs: typeof getExpandedDirs === "function" ? getExpandedDirs() : [],
            activeFilePath: currentFilePath,
          },
          ui: {
            sidebarWidth: parseInt(sidebar.style.width) || 280,
            scmFlex: parseInt(getComputedStyle(fileTree).flexGrow) || 3,
            terminalFlex: _terminalFlex,
            theme: currentTheme,
          },
        };
        await fetch("/api/workspace", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(payload),
        });
      } catch (e) {}
    }, 500);
  }

  async function loadWorkspace() {
    try {
      var resp = await fetch("/api/workspace");
      var state = await resp.json();
      if (state.error) return;

      // 恢复侧边栏宽度
      if (state.ui && state.ui.sidebarWidth) {
        sidebar.style.width = state.ui.sidebarWidth + "px";
      }
      // 恢复 SCM flex
      if (state.ui && state.ui.scmFlex) {
        fileTree.style.flex = state.ui.scmFlex + " 1 0";
      }
      // 恢复终端 flex
      if (state.ui && state.ui.terminalFlex && terminalPanel) {
        _terminalFlex = state.ui.terminalFlex;
        terminalPanel.style.flex = _terminalFlex + " 1 0";
      }
      // 恢复展开的目录
      if (state.fileTree && state.fileTree.expandedDirs && state.fileTree.expandedDirs.length > 0) {
        localStorage.setItem(CACHE_PREFIX + "state:expanded", JSON.stringify(state.fileTree.expandedDirs));
      }
      // 恢复活动文件
      if (state.fileTree && state.fileTree.activeFilePath) {
        currentFilePath = state.fileTree.activeFilePath;
      }
      _workspaceLoaded = true;
    } catch (e) {}
  }

  // 钩子：保存展开状态时同步到工作区
  if (typeof addExpandedDir === "function") {
    var _realAdd = addExpandedDir;
    addExpandedDir = function(p) { _realAdd(p); syncWorkspace(); };
  }
  if (typeof removeExpandedDir === "function") {
    var _realRemove = removeExpandedDir;
    removeExpandedDir = function(p) { _realRemove(p); syncWorkspace(); };
  }

  // ---------- 主题切换 ----------
  var themeBtn = document.getElementById("statusbar-theme");
  var currentTheme = localStorage.getItem("theme") || "dark";

  function applyTheme(theme) {
    document.documentElement.setAttribute("data-theme", theme);
    currentTheme = theme;
    localStorage.setItem("theme", theme);
    if (themeBtn) themeBtn.textContent = theme === "dark" ? "🌙" : "☀️";
  }

  applyTheme(currentTheme);

  if (themeBtn) {
    themeBtn.addEventListener("click", function() {
      applyTheme(currentTheme === "dark" ? "light" : "dark");
    });
  }

  // ---------- 启动 ----------
  async function init() {
    await detectGit();
    await loadWorkspace();
    loadTree();
    // 恢复上次打开的文件
    if (currentFilePath) {
      fetch("/api/stat?path=" + encodeURIComponent(currentFilePath))
        .then(function(resp) { return resp.json(); })
        .then(function(data) {
          if (!data.error) {
            currentFileMtime = data.mtime;
            showPreview(currentFilePath.split("/").pop());
            renderPreview(currentFilePath, currentFilePath.split("/").pop());
            startPolling(currentFilePath, currentFileMtime);
          }
        })
        .catch(function() {});
    }
  }

  init();
})();
