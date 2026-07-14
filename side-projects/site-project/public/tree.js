// ========== 文件树组件 (懒加载 + 缓存) ==========

var fileIcons = {
  md: "📝", js: "🟨", ts: "🟦", jsx: "⚛️", tsx: "⚛️",
  html: "🌐", htm: "🌐", css: "🎨", scss: "🎨", less: "🎨",
  json: "📋", xml: "📋", yaml: "⚙️", yml: "⚙️", toml: "⚙️",
  py: "🐍", rb: "💎", go: "🔵", rs: "🦀", java: "☕",
  c: "⚙️", cpp: "⚙️", h: "⚙️", hpp: "⚙️",
  sh: "💻", bash: "💻", zsh: "💻", sql: "🗄️",
  svg: "🖼️", png: "🖼️", jpg: "🖼️", jpeg: "🖼️", gif: "🖼️", webp: "🖼️",
  pdf: "📄", txt: "📃", graphql: "◈",
};

function getFileIcon(name, isDir) {
  if (isDir) return "📁";
  var ext = name.includes(".") ? name.split(".").pop().toLowerCase() : "";
  return fileIcons[ext] || "📄";
}

function getFileExt(name) {
  if (!name.includes(".")) return "";
  return name.split(".").pop().toLowerCase();
}

var currentActiveEl = null;

function setActive(el) {
  if (currentActiveEl) currentActiveEl.classList.remove("active");
  el.classList.add("active");
  currentActiveEl = el;
}

// ---------- 渲染单个节点 ----------
function renderNode(node, level) {
  level = level || 0;
  var nodeDiv = document.createElement("div");
  nodeDiv.className = "tree-node";

  var item = document.createElement("div");
  item.className = "tree-item";
  item.style.paddingLeft = (12 + level * 16) + "px";
  item.dataset.type = node.type;
  item.dataset.path = node.path;
  if (node.type === "file") {
    item.dataset.ext = getFileExt(node.name);
  }

  // 折叠/展开箭头
  var toggle = document.createElement("span");
  toggle.className = "tree-toggle";
  if (node.type === "directory") {
    toggle.textContent = "▶";
    if (!node.hasChildren) {
      toggle.classList.add("empty");
    }
  } else {
    toggle.classList.add("empty");
  }
  item.appendChild(toggle);

  // 图标
  var icon = document.createElement("span");
  icon.className = "tree-icon";
  icon.textContent = getFileIcon(node.name, node.type === "directory");
  item.appendChild(icon);

  // 名称
  var nameEl = document.createElement("span");
  nameEl.className = "tree-name";
  nameEl.textContent = node.name;
  item.appendChild(nameEl);

  nodeDiv.appendChild(item);

  // 子节点容器
  var childrenDiv = null;
  if (node.type === "directory") {
    childrenDiv = document.createElement("div");
    childrenDiv.className = "tree-children collapsed";
    nodeDiv.appendChild(childrenDiv);
  }

  // 是否已加载过子节点
  var childrenLoaded = false;
  // 正在加载标记
  var loading = false;

  // 点击事件
  item.addEventListener("click", function(e) {
    e.stopPropagation();
    if (node.type === "directory") {
      if (loading) return;

      var isExpanded = toggle.classList.contains("expanded");
      if (isExpanded) {
        // 折叠
        toggle.classList.remove("expanded");
        if (childrenDiv) childrenDiv.classList.add("collapsed");
        removeExpandedDir(node.path);
      } else {
        // 展开
        toggle.classList.add("expanded");
        if (childrenDiv) childrenDiv.classList.remove("collapsed");
        addExpandedDir(node.path);

        // 懒加载子节点
        if (!childrenLoaded && node.hasChildren) {
          loadChildren(node.path, childrenDiv, function() {
            childrenLoaded = true;
          });
        }
      }
    } else {
      setActive(item);
      window.dispatchEvent(new CustomEvent("file-select", {
        detail: { path: node.path, name: node.name }
      }));
    }
  });

  return nodeDiv;
}

// ---------- 懒加载子节点 ----------
function loadChildren(dirPath, container, callback) {
  // 先查缓存
  var cacheKeyName = "tree:" + dirPath;
  var cached = cacheGet(cacheKeyName);
  if (cached) {
    renderChildren(cached, container);
    if (callback) callback();
    return;
  }

  // 显示加载中
  container.innerHTML = '<div style="padding:4px 0 4px ' + (40) + 'px;color:var(--text-muted);font-size:12px;">加载中...</div>';

  fetch("/api/tree?path=" + encodeURIComponent(dirPath))
    .then(function(resp) { return resp.json(); })
    .then(function(nodes) {
      // 存入缓存
      cacheSet(cacheKeyName, nodes);
      renderChildren(nodes, container);
      if (callback) callback();
    })
    .catch(function(err) {
      container.innerHTML = '<div style="padding:4px 0 4px 40px;color:#cf222e;font-size:12px;">加载失败</div>';
    });
}

// ---------- 渲染子节点列表 ----------
function renderChildren(nodes, container) {
  container.innerHTML = "";
  if (!nodes || nodes.length === 0) {
    container.innerHTML = '<div style="padding:4px 0 4px 40px;color:var(--text-muted);font-size:12px;">空目录</div>';
    return;
  }
  // 计算当前层级（基于父节点的缩进）
  var parentItem = container.parentElement.querySelector(".tree-item");
  var parentPadding = parentItem ? parseInt(parentItem.style.paddingLeft) || 12 : 12;
  var level = Math.floor((parentPadding - 12) / 16) + 1;

  for (var i = 0; i < nodes.length; i++) {
    var childEl = renderNode(nodes[i], level);
    container.appendChild(childEl);
  }
}

// ---------- 渲染根级树 ----------
function renderTree(container, nodes) {
  container.innerHTML = "";
  if (!nodes || nodes.length === 0) {
    container.innerHTML = '<div style="padding:20px;color:var(--text-muted);text-align:center;">空目录</div>';
    return;
  }
  for (var i = 0; i < nodes.length; i++) {
    var el = renderNode(nodes[i], 0);
    container.appendChild(el);
  }
}

// ---------- 折叠/展开全部 ----------
function collapseAll() {
  document.querySelectorAll(".tree-toggle.expanded").forEach(function(t) {
    t.classList.remove("expanded");
    var children = t.parentElement && t.parentElement.parentElement
      ? t.parentElement.parentElement.querySelector(".tree-children")
      : null;
    if (children) children.classList.add("collapsed");
  });
}

function expandAll() {
  document.querySelectorAll(".tree-toggle:not(.empty):not(.expanded)").forEach(function(t) {
    t.classList.add("expanded");
    var nodeDiv = t.parentElement && t.parentElement.parentElement;
    var children = nodeDiv ? nodeDiv.querySelector(".tree-children") : null;
    if (children) {
      children.classList.remove("collapsed");
      // 触发懒加载
      var item = t.parentElement;
      if (item) {
        var path = item.dataset.path;
        if (path) {
          var cacheKeyName = "tree:" + path;
          if (!cacheGet(cacheKeyName)) {
            loadChildren(path, children);
          }
        }
      }
    }
    // 保存展开状态
    var p = t.parentElement && t.parentElement.dataset.path;
    if (p) addExpandedDir(p);
  });
}

// ---------- 展开状态持久化 ----------
var EXPANDED_KEY = "state:expanded";

function getExpandedDirs() {
  try {
    var raw = localStorage.getItem(CACHE_PREFIX + EXPANDED_KEY);
    return raw ? JSON.parse(raw) : [];
  } catch (e) {
    return [];
  }
}

function addExpandedDir(dirPath) {
  var dirs = getExpandedDirs();
  if (dirs.indexOf(dirPath) === -1) {
    dirs.push(dirPath);
    localStorage.setItem(CACHE_PREFIX + EXPANDED_KEY, JSON.stringify(dirs));
  }
}

function removeExpandedDir(dirPath) {
  var dirs = getExpandedDirs();
  var idx = dirs.indexOf(dirPath);
  if (idx !== -1) {
    dirs.splice(idx, 1);
    localStorage.setItem(CACHE_PREFIX + EXPANDED_KEY, JSON.stringify(dirs));
  }
}

// ---------- 恢复上次展开状态（app.js 在树渲染后调用）----------
function restoreExpandedState() {
  var dirs = getExpandedDirs();
  if (dirs.length === 0) return;

  // 按路径深度分组，逐层展开
  var levels = {};
  for (var i = 0; i < dirs.length; i++) {
    var depth = dirs[i].split("/").length;
    if (!levels[depth]) levels[depth] = [];
    levels[depth].push(dirs[i]);
  }

  var depths = Object.keys(levels).sort(function(a, b) { return a - b; });
  var delay = 0;

  for (var d = 0; d < depths.length; d++) {
    var paths = levels[depths[d]];
    for (var p = 0; p < paths.length; p++) {
      (function(dirPath, dly) {
        setTimeout(function() {
          expandDirByPath(dirPath);
        }, dly);
      })(paths[p], delay);
    }
    delay += 300; // 每层间隔 300ms 等子节点加载
  }
}

// 按路径展开单个目录（逐级触发懒加载）
function expandDirByPath(dirPath) {
  var parts = dirPath.split("/");
  var currentEl = document.getElementById("file-tree");
  if (!currentEl) return;

  for (var i = 0; i < parts.length; i++) {
    var targetPath = parts.slice(0, i + 1).join("/");
    var items = currentEl.querySelectorAll(".tree-item[data-type='directory']");
    var found = false;

    for (var j = 0; j < items.length; j++) {
      if (items[j].dataset.path === targetPath) {
        var nodeDiv = items[j].parentElement;
        var toggle = items[j].querySelector(".tree-toggle");
        var children = nodeDiv.querySelector(".tree-children");

        if (toggle && !toggle.classList.contains("expanded")) {
          toggle.classList.add("expanded");
          if (children) children.classList.remove("collapsed");
          addExpandedDir(targetPath);

          // 如果还没加载子节点，触发懒加载
          if (!children || children.querySelector(".tree-item")) {
            // 已有子节点或正在加载
          } else {
            loadChildren(targetPath, children);
          }
        }

        if (children) currentEl = children;
        found = true;
        break;
      }
    }
    if (!found) break;
  }
}
