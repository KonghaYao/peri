// ========== Git Graph 组件 ==========

(function() {

var LANE_W = window.GitGraphLayout.LANE_W;
var ROW_H = window.GitGraphLayout.ROW_H;
var DOT_R = window.GitGraphLayout.DOT_R;
var LINE_W = window.GitGraphLayout.LINE_W;

var LANE_COLORS = window.GitGraphLayout.LANE_COLORS;

// ---------- SVG 图标 (来自 MIT 许可的 Octicons) ----------
var GC_ICONS = {
  branch: '<svg viewBox="0 0 10 16" width="10" height="16"><path fill-rule="evenodd" d="M10 5c0-1.11-.89-2-2-2a1.993 1.993 0 0 0-1 3.72v.3c-.02.52-.23.98-.63 1.38-.4.4-.86.61-1.38.63-.83.02-1.48.16-2 .45V4.72a1.993 1.993 0 0 0-1-3.72C.88 1 0 1.89 0 3a2 2 0 0 0 1 1.72v6.56c-.59.35-1 .99-1 1.72 0 1.11.89 2 2 2 1.11 0 2-.89 2-2 0-.53-.2-1-.53-1.36.09-.06.48-.41.59-.47.25-.11.56-.17.94-.17 1.05-.05 1.95-.45 2.75-1.25S8.95 7.77 9 6.73h-.02C9.59 6.37 10 5.73 10 5z"/></svg>',
  tag: '<svg viewBox="0 0 15 16" width="15" height="16"><path fill-rule="evenodd" d="M7.73 1.73C7.26 1.26 6.62 1 5.96 1H3.5C2.13 1 1 2.13 1 3.5v2.47c0 .66.27 1.3.73 1.77l6.06 6.06c.39.39 1.02.39 1.41 0l4.59-4.59a.996.996 0 0 0 0-1.41L7.73 1.73zM2.38 7.09c-.31-.3-.47-.7-.47-1.13V3.5c0-.88.72-1.59 1.59-1.59h2.47c.42 0 .83.16 1.13.47l6.14 6.13-4.73 4.73-6.13-6.15zM3.01 3h2v2H3V3h.01z"/></svg>',
  stash: '<svg viewBox="0 0 14 16" width="14" height="16"><path fill-rule="evenodd" d="M14 9l-1.13-7.14c-.08-.48-.5-.86-1-.86H2.13c-.5 0-.92.38-1 .86L0 9v5c0 .55.45 1 1 1h12c.55 0 1-.45 1-1V9zm-3.28.55l-.44.89c-.17.34-.52.56-.91.56H4.61c-.38 0-.72-.22-.89-.55l-.44-.91c-.17-.33-.52-.55-.89-.55H1l1-7h10l1 7h-1.38c-.39 0-.73.22-.91.55l.01.01z"/></svg>'
};

var graphCommits = [];
var knownRemotes = [];  // remote 名称列表，用于区分远程分支
var knownRemoteBranches = []; // 精确的远程分支列表 (from git branch -r)
var activeHash = null;

// 分页状态
var moreAvailable = false;
var currentMaxCommits = 200;
var isLoadingMore = false;

// 对齐模式
var alignedMode = false;  // 文本是否对齐到 graph 节点

// 搜索状态
var findIndex = -1;     // 当前匹配索引
var findMatches = [];   // 匹配的 commit 索引列表
var findTerm = "";      // 当前搜索词

// Tooltip 状态
var graphTooltipEl = null;
var currentGraphRef = null;  // 上次 drawGraph 的 Graph 对象引用

// ---------- 解析 refs ----------
function parseRefs(refs, remotes, remoteBranches) {
  var result = [];
  for (var i = 0; i < refs.length; i++) {
    var ref = refs[i];
    if (!ref) continue;

    if (ref.startsWith("HEAD -> ")) {
      result.push({ type: "head", label: "HEAD" });
      result.push({ type: "branch", label: ref.slice(7) });
    } else if (ref.startsWith("tag: ")) {
      result.push({ type: "tag", label: ref.slice(5) });
    } else if (ref === "refs/stash" || ref.match(/^stash@\{\d+\}$/)) {
      // git 内部 ref: refs/stash；或 reflog 写法: stash@{N}
      result.push({ type: "stash", label: ref === "refs/stash" ? "stash" : ref });
    } else {
      // 精确匹配："<remote>/HEAD" 或远程分支列表中的条目
      var isRemote = false;
      for (var r = 0; r < remoteBranches.length; r++) {
        if (ref === remoteBranches[r]) { isRemote = true; break; }
      }
      // 特殊：origin/HEAD 是 symref，过滤掉
      var isHeadSymref = false;
      for (var r = 0; r < remotes.length; r++) {
        if (ref === remotes[r] + "/HEAD") { isHeadSymref = true; break; }
      }
      if (!isHeadSymref) {
        result.push({ type: isRemote ? "remote" : "branch", label: ref });
      }
    }
  }
  return result;
}

// ---------- Canvas 绘制（基于布局引擎输出）----------
function drawGraph(canvas, commits) {
  // 1. 运行布局引擎
  var graph = new window.GitGraphLayout.Graph(LANE_COLORS);
  graph.loadCommits(commits);
  currentGraphRef = graph;  // 保存引用供 tooltip 使用
  var vertices = graph.vertices;
  var branches = graph.branches;

  // 2. 计算画布尺寸
  var canvasW = graph.getContentWidth(LANE_W);
  var totalH = vertices.length * ROW_H;
  var dpr = window.devicePixelRatio || 1;

  canvas.width = canvasW * dpr;
  canvas.height = totalH * dpr;
  canvas.style.width = canvasW + "px";
  canvas.style.height = totalH + "px";

  var ctx = canvas.getContext("2d");
  ctx.scale(dpr, dpr);
  ctx.clearRect(0, 0, canvasW, totalH);

  // 3. 为每条分支绘制线条
  for (var bi = 0; bi < branches.length; bi++) {
    var branch = branches[bi];
    var colour = LANE_COLORS[branch.colour % LANE_COLORS.length];
    var lines = branch.lines;

    if (lines.length === 0) continue;

    ctx.strokeStyle = colour;
    ctx.lineWidth = LINE_W;
    ctx.lineCap = "round";
    ctx.lineJoin = "round";

    // 简化：合并连续的垂直直线
    var simplified = [lines[0]];
    for (var li = 1; li < lines.length; li++) {
      var prev = simplified[simplified.length - 1];
      var curr = lines[li];
      // 如果前后两段都是垂直的且在同一列，合并
      if (prev.p1.x === prev.p2.x && curr.p1.x === curr.p2.x
          && prev.p2.x === curr.p1.x && prev.p2.y === curr.p1.y) {
        prev.p2.y = curr.p2.y;
      } else {
        simplified.push(curr);
      }
    }

    // 逐段绘制
    for (var li = 0; li < simplified.length; li++) {
      var line = simplified[li];
      var x1 = line.p1.x * LANE_W + LANE_W / 2;
      var y1 = line.p1.y * ROW_H + ROW_H / 2;
      var x2 = line.p2.x * LANE_W + LANE_W / 2;
      var y2 = line.p2.y * ROW_H + ROW_H / 2;

      // 检查是否连接到 uncommitted 或 stash 节点
      var isUncommittedLine = false;
      var isStashLine = false;
      if (commits[line.p1.y] && commits[line.p1.y].uncommitted) isUncommittedLine = true;
      if (commits[line.p1.y] && commits[line.p1.y].stash) isStashLine = true;
      if (commits[line.p2.y] && commits[line.p2.y].uncommitted) isUncommittedLine = true;
      if (commits[line.p2.y] && commits[line.p2.y].stash) isStashLine = true;

      if (isStashLine) {
        ctx.strokeStyle = "#9c27b0";
        ctx.setLineDash([3, 3]);
      } else if (isUncommittedLine) {
        ctx.strokeStyle = "#808080";
        ctx.setLineDash([3, 3]);
      } else {
        ctx.strokeStyle = colour;
        ctx.setLineDash([]);
      }

      ctx.beginPath();

      if (x1 === x2) {
        // 垂直线
        ctx.moveTo(x1, y1);
        ctx.lineTo(x2, y2);
      } else {
        // 水平过渡线：贝塞尔曲线
        var d = ROW_H * 0.8;  // 过渡弧度
        ctx.moveTo(x1, y1);
        if (line.lockedFirst) {
          // 锁定起点：曲线从起点开始弯曲
          ctx.bezierCurveTo(x1, y1 + d, x2, y2 - d, x2, y2);
        } else {
          // 锁定终点：曲线弯曲到终点
          ctx.bezierCurveTo(x1, y1 + d, x2, y2 - d, x2, y2);
        }
      }

      ctx.stroke();
    }
  }

  // 4. 绘制提交节点（圆点）
  for (var vi = 0; vi < vertices.length; vi++) {
    var vertex = vertices[vi];
    if (vertex.isNotOnBranch()) continue;

    var commit = commits[vi] || {};
    var colour = LANE_COLORS[vertex.getBranch().colour % LANE_COLORS.length];

    if (commit.uncommitted) {
      // Uncommitted: 空心圆 + 虚线色
      colour = "#808080";
    }

    var cx = vertex.getPoint().x * LANE_W + LANE_W / 2;
    var cy = vertex.getPoint().y * ROW_H + ROW_H / 2;

    if (commit.uncommitted) {
      // Uncommitted: 空心圆，白色填充
      ctx.fillStyle = "#ffffff";
      ctx.beginPath();
      ctx.arc(cx, cy, DOT_R + 1, 0, Math.PI * 2);
      ctx.fill();
      ctx.strokeStyle = colour;
      ctx.lineWidth = LINE_W;
      ctx.setLineDash([2, 2]);
      ctx.stroke();
      ctx.setLineDash([]);
    } else if (commit.stash) {
      // Stash: 紫色菱形
      ctx.fillStyle = "#9c27b0";
      ctx.beginPath();
      var s = DOT_R + 1.5;
      ctx.moveTo(cx, cy - s);
      ctx.lineTo(cx + s, cy);
      ctx.lineTo(cx, cy + s);
      ctx.lineTo(cx - s, cy);
      ctx.closePath();
      ctx.fill();
      ctx.strokeStyle = "rgba(255,255,255,0.5)";
      ctx.lineWidth = 1.5;
      ctx.stroke();
    } else {
      ctx.fillStyle = colour;
      ctx.beginPath();
      ctx.arc(cx, cy, DOT_R, 0, Math.PI * 2);
      ctx.fill();

      if (vertex.isCurrent) {
        // HEAD commit：空心圆（白色边框 + 色环）
        ctx.strokeStyle = "#ffffff";
        ctx.lineWidth = 2.5;
        ctx.stroke();
        ctx.strokeStyle = colour;
        ctx.lineWidth = LINE_W;
        ctx.stroke();
      } else {
        // 普通 commit：白色分隔环 + 色环
        ctx.strokeStyle = "#ffffff";
        ctx.lineWidth = 2.5;
        ctx.stroke();
        ctx.strokeStyle = colour;
        ctx.lineWidth = LINE_W;
        ctx.stroke();
      }
    }
  }
}

// ---------- 相对时间 ----------
function relativeTime(dateStr) {
  var now = Date.now();
  var date = Date.parse(dateStr);
  if (isNaN(date)) return dateStr;
  var diff = now - date;
  var s = Math.floor(diff / 1000);
  var m = Math.floor(s / 60);
  var h = Math.floor(m / 60);
  var d = Math.floor(h / 24);
  var w = Math.floor(d / 7);
  var mo = Math.floor(d / 30);
  var y = Math.floor(d / 365);

  if (y > 0) return y + ' year' + (y > 1 ? 's' : '') + ' ago';
  if (mo > 0) return mo + ' month' + (mo > 1 ? 's' : '') + ' ago';
  if (w > 0) return w + ' week' + (w > 1 ? 's' : '') + ' ago';
  if (d > 0) return d + ' day' + (d > 1 ? 's' : '') + ' ago';
  if (h > 0) return h + ' hour' + (h > 1 ? 's' : '') + ' ago';
  if (m > 0) return m + ' minute' + (m > 1 ? 's' : '') + ' ago';
  return 'just now';
}

// 短日期格式 "Jun 25" 或 "2024-06-25"
function formatDate(dateStr) {
  if (!dateStr) return "";
  var d = new Date(dateStr + "T00:00:00");
  if (isNaN(d.getTime())) return dateStr;
  var months = ["Jan","Feb","Mar","Apr","May","Jun","Jul","Aug","Sep","Oct","Nov","Dec"];
  var now = new Date();
  if (d.getFullYear() === now.getFullYear()) {
    return months[d.getMonth()] + " " + d.getDate();
  }
  return d.getFullYear() + "-" + String(d.getMonth()+1).padStart(2,"0") + "-" + String(d.getDate()).padStart(2,"0");
}

// 消息分类：返回 CSS class、icon、简短说明
function classifyMessage(subject) {
  if (!subject) return { type: "", icon: "", label: "" };

  var s = subject.trim();

  // Merge 类 (不区分大小写检测)
  var lower = s.toLowerCase();
  if (/^merge\s+(branch|pull\s+request|remote-tracking|tag)\b/i.test(s)) {
    // 提取被合并的分支名
    var m = s.match(/^[Mm]erge\s+(?:branch|pull\s+request|remote-tracking\s+branch|tag)\s+['"]([^'"]+)['"]/);
    var branch = m ? m[1] : "";
    return { type: "gc-msg-merge", icon: "\u21C4", label: branch || "Merge" };
  }
  if (/^merge\s+(?:[a-zA-Z0-9._/-]+\s+)?(?:into|from)\b/i.test(s)) {
    return { type: "gc-msg-merge", icon: "\u21C4", label: "Merge" };
  }

  // Revert 类
  if (/^[Rr]evert\s+["""]/.test(s) || /^[Rr]evert:?\s/.test(s)) {
    return { type: "gc-msg-revert", icon: "\u21A9", label: "Revert" };
  }

  // Cherry-pick 类
  if (/^[Cc]herry[- ]pick[:\s]/i.test(s)) {
    return { type: "gc-msg-cherrypick", icon: "\u2691", label: "Cherry-pick" };
  }

  // Squash 类
  if (/^[Ss]quash\b/i.test(s)) {
    return { type: "gc-msg-squash", icon: "\u2261", label: "Squash" };
  }

  return { type: "", icon: "", label: "" };
}

// ---------- 远程跟踪合并（参照 vscode-git-graph getBranchLabels）----------
// 将远程分支合并到同名本地分支的胶囊中
function mergeRemoteTracking(refsList) {
  var heads = [];      // { name, remotes[] }
  var headLookup = {};
  var pureRemotes = [];
  var otherRefs = [];  // HEAD, tags, stash 等不参与合并的 refs

  // 第一趟：收集所有 branch、HEAD、tag、stash
  for (var i = 0; i < refsList.length; i++) {
    var ref = refsList[i];
    if (ref.type === "branch") {
      heads.push({ name: ref.label, remotes: [] });
      headLookup[ref.label] = heads.length - 1;
    } else if (ref.type !== "remote") {
      otherRefs.push(ref);
    }
    // remote 留到第二趟处理
  }

  // 第二趟：将所有 remote 分支匹配到已收集的本地分支
  for (var i = 0; i < refsList.length; i++) {
    var ref = refsList[i];
    if (ref.type !== "remote") continue;

    var slashIdx = ref.label.indexOf("/");
    if (slashIdx > 0) {
      var remoteName = ref.label.slice(0, slashIdx);
      var branchName = ref.label.slice(slashIdx + 1);
      if (typeof headLookup[branchName] === "number") {
        heads[headLookup[branchName]].remotes.push(remoteName);
        continue;  // 已合并，跳过
      }
    }
    pureRemotes.push(ref);
  }

  return { heads: heads, remotes: pureRemotes, others: otherRefs };
}

// ---------- 渲染 Commit 列表 ----------
function renderCommitList(commits) {
  var listEl = document.getElementById("graph-commits");
  if (!listEl) return;

  var html = "";

  // 获取每个 commit 对应的 graph lane 颜色索引 + x 位置
  var vertexColours = currentGraphRef ? currentGraphRef.getVertexColours() : [];
  var vertexXPositions = currentGraphRef ? currentGraphRef.getVertexXPositions() : [];

  for (var row = 0; row < commits.length; row++) {
    var c = commits[row];
    var colourIdx = vertexColours[row];
    var colorHex = (typeof colourIdx === "number" && colourIdx >= 0)
      ? LANE_COLORS[colourIdx % LANE_COLORS.length]
      : "transparent";

    if (c.uncommitted) {
      // Uncommitted 行：特殊样式
      html += '<div class="gc-row gc-uncommitted' + (alignedMode ? ' gc-aligned' : '') + '" style="height:' + ROW_H + 'px;' +
        (alignedMode ? 'padding-left:' + (vertexXPositions[0] * LANE_W + LANE_W) + 'px;' : '') + '"' +
        ' data-hash="UNCOMMITTED" title="未提交的工作区变更">' +
        '<span class="gc-color-bar"></span>' +
        '<span class="gc-refs"></span>' +
        '<span class="gc-subject">' + escapeHtml(c.subject) + '</span>' +
        '<span class="gc-author"></span>' +
        '<span class="gc-date">' + escapeHtml(c.uncommittedStats || "") + '</span>' +
        '<span class="gc-hash">UNCOMMITTED</span>' +
        '</div>';
      continue;
    }

    if (c.stash) {
      // Stash 行：紫色调
      var stashLabel = "stash@{" + (typeof c.stashIndex === "number" ? c.stashIndex : "") + "}";
      var xPosSt = vertexXPositions[row] || 0;
      var alignPadSt = alignedMode ? "padding-left:" + (xPosSt * LANE_W + LANE_W) + "px;" : "";

      html += '<div class="gc-row gc-stash' + (alignedMode ? ' gc-aligned' : '') + '" style="height:' + ROW_H + 'px;' + alignPadSt + ';--gc-lane-color:#9c27b0"' +
        ' data-hash="' + escapeHtml(c.hash) + '"' +
        ' data-branch="" title="Stash: ' + escapeHtml(c.subject) + '">' +
        '<span class="gc-color-bar" style="background:#9c27b0;opacity:0.35"></span>' +
        '<span class="gc-refs"><span class="gc-ref gc-ref-stash">' +
        GC_ICONS.stash +
        '<span class="gc-ref-name">' + escapeHtml(stashLabel) + '</span></span></span>' +
        '<span class="gc-subject">' + escapeHtml(c.subject) + '</span>' +
        '<span class="gc-author">' + escapeHtml(c.author) + '</span>' +
        '<span class="gc-date">' + formatDate(c.date) + '</span>' +
        '<span class="gc-hash">' + escapeHtml(c.shortHash) + '</span>' +
        '</div>';
      continue;
    }

    var refs = parseRefs(c.refs, knownRemotes, knownRemoteBranches);
    var branchName = "";
    for (var r = 0; r < refs.length; r++) {
      if (refs[r].type === "branch") { branchName = refs[r].label; break; }
    }
    var currentBranch = window._currentBranch || "";

    // 合并远程跟踪到本地分支胶囊
    var merged = mergeRemoteTracking(refs);

    var refsHtml = "";
    // 1) HEAD 和 tag 等非分支 refs
    for (var r = 0; r < merged.others.length; r++) {
      var ref = merged.others[r];
      var iconHtml = "";
      if (ref.type === "head") { iconHtml = GC_ICONS.branch; }
      else if (ref.type === "tag") { iconHtml = GC_ICONS.tag; }
      else if (ref.type === "stash") { iconHtml = GC_ICONS.stash; }
      else { iconHtml = GC_ICONS.branch; }
      refsHtml += '<span class="gc-ref gc-ref-' + ref.type + '" data-branch="' + escapeHtml(ref.label) + '">' +
        iconHtml +
        '<span class="gc-ref-name">' + escapeHtml(ref.label) + '</span></span>';
    }
    // 2) 本地分支 (含远程跟踪子标签)
    for (var r = 0; r < merged.heads.length; r++) {
      var head = merged.heads[r];
      var isCurrentBranch = (currentBranch !== "" && head.name === currentBranch);
      var activeClass = isCurrentBranch ? " gc-ref-active" : "";
      refsHtml += '<span class="gc-ref gc-ref-branch' + activeClass + '" data-branch="' + escapeHtml(head.name) + '" title="' + (isCurrentBranch ? "当前分支 — 点击切换" : "点击切换分支") + '">' +
        GC_ICONS.branch +
        '<span class="gc-ref-name">' + escapeHtml(head.name) + '</span>';
      for (var k = 0; k < head.remotes.length; k++) {
        refsHtml += '<span class="gc-ref-remote-track">' + escapeHtml(head.remotes[k]) + '</span>';
      }
      refsHtml += '</span>';
    }
    // 3) 纯远程分支 (未匹配到本地分支的)
    for (var r = 0; r < merged.remotes.length; r++) {
      var remoteRef = merged.remotes[r];
      refsHtml += '<span class="gc-ref gc-ref-remote" data-branch="' + escapeHtml(remoteRef.label) + '">' +
        GC_ICONS.branch +
        '<span class="gc-ref-name">' + escapeHtml(remoteRef.label) + '</span></span>';
    }

    var selected = (c.hash === activeHash) ? " gc-selected" : "";
    var alignedClass = alignedMode ? " gc-aligned" : "";
    var msgClass = classifyMessage(c.subject);
    var msgTypeClass = msgClass.type ? " " + msgClass.type : "";
    var xPos = vertexXPositions[row] || 0;
    var alignPadding = alignedMode ? "padding-left:" + (xPos * LANE_W + LANE_W) + "px;" : "";

    html += '<div class="gc-row' + selected + alignedClass + msgTypeClass + '" style="height:' + ROW_H + 'px;' + alignPadding + ';--gc-lane-color:' + colorHex + '"' +
      ' data-hash="' + escapeHtml(c.hash) + '"' +
      ' data-short-hash="' + escapeHtml(c.shortHash) + '"' +
      ' data-branch="' + escapeHtml(branchName) + '"' +
      ' data-color="' + colourIdx + '"' +
      ' title="' + escapeHtml(c.author + ' · ' + c.date + '\n' + c.subject) + '">' +
      '<span class="gc-color-bar" style="background:' + colorHex + '"></span>' +
      '<span class="gc-refs">' + refsHtml + '</span>' +
      '<span class="gc-subject">' + escapeHtml(c.subject) + '</span>' +
      '<span class="gc-author">' + escapeHtml(c.author) + '</span>' +
      '<span class="gc-date">' + formatDate(c.date) + '</span>' +
      '<span class="gc-hash">' + escapeHtml(c.shortHash) + '</span>' +
      '</div>';
  }

  listEl.innerHTML = html;
  listEl.style.minHeight = (commits.length * ROW_H) + "px";

  // Load More 按钮
  if (moreAvailable) {
    var loadMoreHtml = '<div id="graph-load-more" class="gc-load-more"' +
      ' style="height:' + ROW_H + 'px"' +
      (isLoadingMore ? ' data-loading="true"' : '') + '>' +
      (isLoadingMore
        ? '<span class="gc-load-more-spinner"></span>Loading...'
        : 'Load More Commits') +
      '</div>';
    listEl.insertAdjacentHTML("beforeend", loadMoreHtml);
    var loadMoreBtn = document.getElementById("graph-load-more");
    if (loadMoreBtn && !isLoadingMore) {
      loadMoreBtn.onclick = function() { loadMoreCommits(); };
    }
  }

  // 左键单击 → 查看 diff（分支 badge 除外）
  listEl.onclick = function(e) {
    // 分支 badge 点击 → 切换分支
    var refEl = e.target.closest(".gc-ref-branch");
    if (refEl) {
      e.stopPropagation();
      var branch = refEl.getAttribute("data-branch");
      if (branch) window._graphCheckoutBranch(branch);
      return;
    }
    // 远程分支 badge 点击 → checkout remote
    var remoteRefEl = e.target.closest(".gc-ref-remote");
    if (remoteRefEl) {
      e.stopPropagation();
      var remoteBranch = remoteRefEl.getAttribute("data-branch");
      if (remoteBranch && confirm("Checkout remote branch '" + remoteBranch + "' as local tracking branch?")) {
        switchRemoteBranch(remoteBranch);
      }
      return;
    }

    var rowEl = e.target.closest(".gc-row");
    if (!rowEl) return;

    var hash = rowEl.dataset.hash;
    if (hash === "UNCOMMITTED") return; // 跳过未提交行
    activeHash = hash;

    var rows = listEl.querySelectorAll(".gc-row");
    for (var i = 0; i < rows.length; i++) {
      rows[i].classList.toggle("gc-selected", rows[i].dataset.hash === hash);
    }

    if (hash) viewCommitDiff(hash);
  };

  // 右键菜单
  listEl.oncontextmenu = function(e) {
    var rowEl = e.target.closest(".gc-row");
    if (!rowEl) return;

    e.preventDefault();
    var hash = rowEl.dataset.hash;
    if (hash === "UNCOMMITTED") return; // 跳过未提交行
    var shortHash = rowEl.dataset.shortHash;
    var branchName = rowEl.dataset.branch || "";
    showContextMenu(e.clientX, e.clientY, hash, shortHash, branchName);
  };
}

// ---------- 右键菜单 ----------
var ctxMenu = null;

function createContextMenu() {
  if (ctxMenu) return;
  ctxMenu = document.createElement("div");
  ctxMenu.id = "graph-context-menu";
  ctxMenu.className = "gc-menu";
  ctxMenu.style.display = "none";
  document.body.appendChild(ctxMenu);

  document.addEventListener("click", function() {
    if (ctxMenu) ctxMenu.style.display = "none";
  });
  document.addEventListener("scroll", function() {
    if (ctxMenu) ctxMenu.style.display = "none";
  }, true);
}

function showContextMenu(x, y, hash, shortHash, branchName) {
  createContextMenu();

  var hasBranch = branchName && branchName.length > 0;

  var items = [
    { label: "Copy Hash", action: "copy", hash: shortHash },
    { type: "sep" },
    { label: "Checkout Commit", action: "checkout", hash: hash },
    { label: "Create Branch here...", action: "create-branch", hash: hash, shortHash: shortHash },
    { label: "Create Tag here...", action: "create-tag", hash: hash, shortHash: shortHash },
    { type: "sep" },
  ];

  // 如果有分支 ref，加上 Merge 选项
  if (hasBranch) {
    items.push({ label: "Merge '" + branchName + "' into current", action: "merge", branch: branchName });
  }

  // 远程操作
  if (knownRemotes.length > 0) {
    if (hasBranch) items.push(
      { label: "Push to origin", action: "push", branch: branchName },
      { label: "Pull from origin", action: "pull" }
    );
  }

  items.push(
    { label: "Cherry Pick", action: "cherry-pick", hash: hash },
    { label: "Revert", action: "revert", hash: hash },
    { type: "sep" },
    { label: "Reset (mixed)", action: "reset", hash: hash, mode: "mixed" },
    { label: "Reset (soft)", action: "reset", hash: hash, mode: "soft" },
    { label: "Reset (hard) ⚠", action: "reset", hash: hash, mode: "hard" }
  );

  var html = "";
  for (var i = 0; i < items.length; i++) {
    var item = items[i];
    if (item.type === "sep") {
      html += '<div class="gc-menu-sep"></div>';
    } else {
      html += '<div class="gc-menu-item' + (item.action === "reset" && item.mode === "hard" ? " gc-menu-danger" : "") + '"' +
        ' data-action="' + item.action + '"' +
        ' data-hash="' + (item.hash || "") + '"' +
        (item.shortHash ? ' data-short-hash="' + item.shortHash + '"' : "") +
        (item.mode ? ' data-mode="' + item.mode + '"' : "") +
        (item.branch ? ' data-branch="' + item.branch + '"' : "") +
        '>' + item.label + '</div>';
    }
  }

  ctxMenu.innerHTML = html;
  ctxMenu.style.display = "block";
  ctxMenu.style.left = x + "px";
  ctxMenu.style.top = y + "px";

  var rect = ctxMenu.getBoundingClientRect();
  if (rect.right > window.innerWidth) {
    ctxMenu.style.left = (x - rect.width) + "px";
  }
  if (rect.bottom > window.innerHeight) {
    ctxMenu.style.top = (y - rect.height) + "px";
  }

  ctxMenu.onclick = function(e) {
    var itemEl = e.target.closest(".gc-menu-item");
    if (!itemEl) return;
    var action = itemEl.dataset.action;
    var h = itemEl.dataset.hash;
    var mode = itemEl.dataset.mode;
    var branch = itemEl.dataset.branch;

    ctxMenu.style.display = "none";

    switch (action) {
      case "copy":
        copyHash(h);
        break;
      case "checkout":
        checkoutCommit(h);
        break;
      case "create-branch":
        createBranch(h);
        break;
      case "create-tag":
        createTag(h);
        break;
      case "merge":
        mergeBranch(branch);
        break;
      case "cherry-pick":
        cherryPick(h);
        break;
      case "revert":
        revertCommit(h);
        break;
      case "reset":
        resetTo(h, mode);
        break;
      case "push":
        pushBranch(branch);
        break;
      case "pull":
        pullBranch();
        break;
    }
  };
}

// ---------- 操作函数 ----------
async function checkoutCommit(hash) {
  if (!confirm("Checkout " + hash + " (Detached HEAD)?")) return;
  try {
    var resp = await fetch("/api/scm/branch", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ branch: hash }),
    });
    var data = await resp.json();
    if (data.success) {
      refreshAll();
    } else {
      alert("Checkout 失败: " + (data.error || "未知错误"));
    }
  } catch (err) {
    alert("请求失败: " + (err.message || String(err)));
  }
}

async function createBranch(hash) {
  var name = prompt("分支名称:", "");
  if (!name) return;
  try {
    var resp = await fetch("/api/scm/create-branch", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name: name, hash: hash }),
    });
    var data = await resp.json();
    if (data.success) {
      refreshGraph();
    } else {
      alert("创建分支失败: " + (data.error || "未知错误"));
    }
  } catch (err) {
    alert("请求失败: " + (err.message || String(err)));
  }
}

async function createTag(hash) {
  var name = prompt("Tag 名称:", "");
  if (!name) return;
  var message = prompt("Tag 信息 (可选):", "");
  try {
    var body = { name: name, hash: hash };
    if (message) body.message = message;
    var resp = await fetch("/api/scm/tag", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });
    var data = await resp.json();
    if (data.success) {
      refreshGraph();
    } else {
      alert("创建 Tag 失败: " + (data.error || "未知错误"));
    }
  } catch (err) {
    alert("请求失败: " + (err.message || String(err)));
  }
}

// ---------- 复制 hash ----------
function copyHash(hash) {
  if (!hash) return;
  if (navigator.clipboard && navigator.clipboard.writeText) {
    navigator.clipboard.writeText(hash).then(function() {
      // 静默复制，不弹提示
    }).catch(function() {
      fallbackCopy(hash);
    });
  } else {
    fallbackCopy(hash);
  }
}

function fallbackCopy(text) {
  var ta = document.createElement("textarea");
  ta.value = text;
  ta.style.position = "fixed";
  ta.style.opacity = "0";
  document.body.appendChild(ta);
  ta.select();
  try { document.execCommand("copy"); } catch (e) {}
  document.body.removeChild(ta);
}

// ---------- Cherry Pick ----------
async function cherryPick(hash) {
  if (!confirm("Cherry-pick commit " + hash + " 到当前分支?")) return;
  try {
    var resp = await fetch("/api/scm/cherry-pick", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ hash: hash }),
    });
    var data = await resp.json();
    if (data.success) {
      refreshAll();
    } else {
      alert("Cherry-pick 失败: " + (data.error || "未知错误"));
    }
  } catch (err) {
    alert("请求失败: " + (err.message || String(err)));
  }
}

// ---------- Revert ----------
async function revertCommit(hash) {
  if (!confirm("Revert commit " + hash + "? 这将创建一个新的 revert commit。")) return;
  try {
    var resp = await fetch("/api/scm/revert", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ hash: hash }),
    });
    var data = await resp.json();
    if (data.success) {
      refreshAll();
    } else {
      alert("Revert 失败: " + (data.error || "未知错误"));
    }
  } catch (err) {
    alert("请求失败: " + (err.message || String(err)));
  }
}

// ---------- Reset ----------
async function resetTo(hash, mode) {
  var modeLabel = mode === "hard" ? "HARD" : mode === "soft" ? "soft" : "mixed";
  var warning = mode === "hard"
    ? "⚠️ 危险操作: git reset --hard " + hash + "\n\n这将丢弃所有工作区和暂存区的更改！\n\n确定要继续吗？"
    : "Reset (" + modeLabel + ") 到 " + hash + "?";
  if (!confirm(warning)) return;
  try {
    var resp = await fetch("/api/scm/reset", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ hash: hash, mode: mode }),
    });
    var data = await resp.json();
    if (data.success) {
      refreshAll();
    } else {
      alert("Reset 失败: " + (data.error || "未知错误"));
    }
  } catch (err) {
    alert("请求失败: " + (err.message || String(err)));
  }
}

// ---------- Checkout 远程分支 ----------
async function switchRemoteBranch(name) {
  try {
    var resp = await fetch("/api/scm/checkout-remote", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ branch: name }),
    });
    var data = await resp.json();
    if (data.success) {
      refreshAll();
    } else {
      alert("Checkout 失败: " + (data.error || "未知错误"));
    }
  } catch (err) {
    alert("请求失败: " + (err.message || String(err)));
  }
}

// ---------- Fetch ----------
async function doFetch() {
  var btn = document.getElementById("graph-fetch-btn");
  if (btn) {
    btn.classList.add("fetching");
    btn.title = "Fetching...";
  }
  try {
    var remote = knownRemotes.length > 0 ? knownRemotes[0] : "";
    var resp = await fetch("/api/scm/fetch", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ remote: remote }),
    });
    var data = await resp.json();
    if (data.success) {
      refreshAll();
    } else {
      alert("Fetch 失败: " + (data.error || "未知错误"));
    }
  } catch (err) {
    alert("请求失败: " + (err.message || String(err)));
  }
  if (btn) {
    btn.classList.remove("fetching");
    btn.title = "Fetch (git fetch --prune)";
  }
}

// ---------- Push ----------
async function pushBranch(branch) {
  if (!confirm("Push '" + branch + "' to origin?")) return;
  try {
    var remote = knownRemotes.length > 0 ? knownRemotes[0] : "origin";
    var resp = await fetch("/api/scm/push", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ remote: remote, branch: branch }),
    });
    var data = await resp.json();
    if (data.success) {
      refreshAll();
    } else {
      alert("Push 失败: " + (data.error || "未知错误"));
    }
  } catch (err) {
    alert("请求失败: " + (err.message || String(err)));
  }
}

// ---------- Pull ----------
async function pullBranch() {
  if (!confirm("Pull from origin?")) return;
  try {
    var remote = knownRemotes.length > 0 ? knownRemotes[0] : "origin";
    var resp = await fetch("/api/scm/pull", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ remote: remote }),
    });
    var data = await resp.json();
    if (data.success) {
      refreshAll();
    } else {
      alert("Pull 失败: " + (data.error || "未知错误"));
    }
  } catch (err) {
    alert("请求失败: " + (err.message || String(err)));
  }
}

// ---------- Merge ----------
async function mergeBranch(branch) {
  if (!confirm("Merge '" + branch + "' 到当前分支?")) return;
  try {
    var resp = await fetch("/api/scm/merge", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ branch: branch }),
    });
    var data = await resp.json();
    if (data.success) {
      refreshAll();
    } else {
      alert("Merge 失败: " + (data.error || "未知错误"));
    }
  } catch (err) {
    alert("请求失败: " + (err.message || String(err)));
  }
}

// 点击分支 badge 切换
window._graphCheckoutBranch = function(branch) {
  if (!confirm("切换到分支 " + branch + "?")) return;
  fetch("/api/scm/branch", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ branch: branch }),
  }).then(function(resp) { return resp.json(); })
    .then(function(data) {
      if (data.success) {
        refreshAll();
      } else {
        alert("切换失败: " + (data.error || "未知错误"));
      }
    })
    .catch(function(err) {
      alert("请求失败: " + (err.message || String(err)));
    });
};

// ---------- 全局刷新 ----------
function refreshAll() {
  if (window.refreshGraph) window.refreshGraph();
  if (window.refreshScm) window.refreshScm();
  // 文件树重新加载
  window.dispatchEvent(new CustomEvent("branch-changed"));
  // 状态栏更新
  if (window.refreshStatusBar) window.refreshStatusBar();
}

// ---------- 查看 Commit Diff ----------
function viewCommitDiff(hash) {
  var preview = document.getElementById("preview");
  preview.innerHTML = '<div class="loading"><div class="spinner"></div></div>';

  fetch("/api/scm/commit-diff?hash=" + encodeURIComponent(hash))
    .then(function(resp) { return resp.json(); })
    .then(function(data) {
      if (data.error) {
        preview.innerHTML = '<div class="error-preview">' +
          '<p>获取 commit 失败: ' + escapeHtml(data.error) + '</p></div>';
        return;
      }

      var diffHtml = "";
      if (data.diff && typeof Diff2Html !== "undefined" && Diff2Html.html) {
        try {
          diffHtml = Diff2Html.html(data.diff, {
            drawFileList: false,
            matching: "lines",
            outputFormat: "line-by-line",
          });
        } catch (e) {
          diffHtml = '<pre class="diff-fallback-pre">' + escapeHtml(data.diff) + '</pre>';
        }
      } else if (data.diff) {
        diffHtml = '<pre class="diff-fallback-pre">' + escapeHtml(data.diff) + '</pre>';
      }

      var scheme = (document.documentElement.dataset.theme || "dark") === "dark" ? " d2h-dark-color-scheme" : "";
      preview.innerHTML =
        '<div class="diff-view' + scheme + '">' +
        '<div class="file-info-bar">' +
        '<div>' +
        '<span class="file-name">' + escapeHtml(data.subject || hash) + '</span>' +
        '</div>' +
        '<span style="color:var(--text-muted);font-size:12px;">' +
        escapeHtml(data.author) + ' · ' + escapeHtml(data.date) + '</span>' +
        '</div>' +
        (diffHtml || '<div style="padding:40px;color:var(--text-muted);text-align:center;">无文件变更</div>') +
        '</div>';
    })
    .catch(function(err) {
      preview.innerHTML = '<div class="error-preview">获取失败: ' +
        escapeHtml(err.message || String(err)) + '</div>';
    });
}

// ---------- 刷新 ----------
async function refreshGraph() {
  currentMaxCommits = 200;
  await fetchAndRenderGraph();
}

// ---------- Load More ----------
async function loadMoreCommits() {
  if (isLoadingMore || !moreAvailable) return;
  isLoadingMore = true;

  // 更新按钮状态
  var loadMoreBtn = document.getElementById("graph-load-more");
  if (loadMoreBtn) {
    loadMoreBtn.setAttribute("data-loading", "true");
    loadMoreBtn.innerHTML = '<span class="gc-load-more-spinner"></span>Loading...';
    loadMoreBtn.onclick = null;
  }

  currentMaxCommits += 100;
  await fetchAndRenderGraph(true);  // append 模式

  isLoadingMore = false;
}

async function fetchAndRenderGraph(append) {
  var canvas = document.getElementById("graph-canvas");
  if (!canvas) return;

  var listEl = document.getElementById("graph-commits");

  try {
    var resp = await fetch("/api/scm/graph?max=" + currentMaxCommits);
    var data = await resp.json();

    if (!data.hasRepo) {
      if (listEl) listEl.innerHTML =
        '<div style="padding:20px;color:var(--text-muted);text-align:center;">不是 Git 仓库</div>';
      return;
    }

    if (data.error) {
      if (listEl) listEl.innerHTML =
        '<div style="padding:20px;color:#cf222e;">错误: ' + escapeHtml(data.error) + '</div>';
      return;
    }

    graphCommits = data.commits || [];
    knownRemotes = data.remotes || [];
    knownRemoteBranches = data.remoteBranches || [];
    moreAvailable = data.moreAvailable || false;

    if (graphCommits.length === 0) {
      if (listEl) listEl.innerHTML =
        '<div style="padding:20px;color:var(--text-muted);text-align:center;">暂无提交记录</div>';
      canvas.style.height = "0px";
      return;
    }

    drawGraph(canvas, graphCommits);
    renderCommitList(graphCommits);

    // 搜索词还在，重新高亮
    if (findTerm) performSearch(findTerm, true);

  } catch (err) {
    if (listEl) listEl.innerHTML =
      '<div style="padding:20px;color:#cf222e;">加载失败: ' +
      escapeHtml(err.message || String(err)) + '</div>';
  }
}

// ---------- Resize 重绘 ----------
function handleResize() {
  if (graphCommits.length > 0) {
    var canvas = document.getElementById("graph-canvas");
    if (canvas && canvas.offsetParent !== null) {
      drawGraph(canvas, graphCommits);
    }
  }
}

// 暴露全局
window.refreshGraph = refreshGraph;
window.drawGraph = drawGraph;

// ---------- Find 搜索 ----------
function initFindWidget() {
  var overlayBody = document.querySelector(".graph-overlay-body");
  if (!overlayBody) return;

  // 创建搜索栏
  var findBar = document.createElement("div");
  findBar.id = "graph-find-bar";
  findBar.style.display = "none";
  findBar.innerHTML =
    '<input id="graph-find-input" type="text" placeholder="Search commits (author, message, hash, refs)..." autocomplete="off">' +
    '<span id="graph-find-count"></span>' +
    '<button id="graph-find-prev" title="上一个 (Shift+Enter)">▲</button>' +
    '<button id="graph-find-next" title="下一个 (Enter)">▼</button>' +
    '<button id="graph-find-close" title="关闭 (Esc)">✕</button>';

  overlayBody.insertBefore(findBar, overlayBody.firstChild);

  var input = document.getElementById("graph-find-input");
  var count = document.getElementById("graph-find-count");
  var prevBtn = document.getElementById("graph-find-prev");
  var nextBtn = document.getElementById("graph-find-next");
  var closeBtn = document.getElementById("graph-find-close");

  var searchTimer;
  input.addEventListener("input", function() {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(function() {
      var term = input.value.trim();
      findTerm = term;
      performSearch(term, false);
    }, 200);
  });

  input.addEventListener("keydown", function(e) {
    if (e.key === "Enter") {
      e.preventDefault();
      if (e.shiftKey) {
        navigateFind(-1);
      } else {
        navigateFind(1);
      }
    } else if (e.key === "Escape") {
      e.preventDefault();
      closeFind();
    }
  });

  prevBtn.addEventListener("click", function() { navigateFind(-1); });
  nextBtn.addEventListener("click", function() { navigateFind(1); });
  closeBtn.addEventListener("click", function() { closeFind(); });

  // Ctrl+F 快捷键
  document.addEventListener("keydown", function(e) {
    if ((e.ctrlKey || e.metaKey) && e.key === "f") {
      // 检查是否在 graph overlay 内
      var overlay = document.getElementById("graph-overlay");
      if (overlay && overlay.style.display !== "none") {
        e.preventDefault();
        openFind();
      }
    }
  });
}

function openFind() {
  var bar = document.getElementById("graph-find-bar");
  var input = document.getElementById("graph-find-input");
  if (bar) bar.style.display = "flex";
  if (input) {
    input.value = findTerm;
    input.focus();
    input.select();
  }
}

function closeFind() {
  var bar = document.getElementById("graph-find-bar");
  if (bar) bar.style.display = "none";
  findTerm = "";
  findMatches = [];
  findIndex = -1;
  // 清除所有高亮
  var rows = document.querySelectorAll("#graph-commits .gc-row.gc-find-match, #graph-commits .gc-row.gc-find-current");
  for (var i = 0; i < rows.length; i++) {
    rows[i].classList.remove("gc-find-match", "gc-find-current");
  }
  var countEl = document.getElementById("graph-find-count");
  if (countEl) countEl.textContent = "";
}

function performSearch(term, silent) {
  var countEl = document.getElementById("graph-find-count");
  var rows = document.querySelectorAll("#graph-commits .gc-row");

  // 清除旧高亮
  for (var i = 0; i < rows.length; i++) {
    rows[i].classList.remove("gc-find-match", "gc-find-current");
  }

  if (!term) {
    findMatches = [];
    findIndex = -1;
    if (countEl) countEl.textContent = "";
    return;
  }

  var lowerTerm = term.toLowerCase();
  findMatches = [];
  findIndex = -1;

  for (var i = 0; i < rows.length; i++) {
    var row = rows[i];
    var text = (row.textContent || "").toLowerCase();
    if (text.indexOf(lowerTerm) !== -1) {
      findMatches.push(i);
      row.classList.add("gc-find-match");
    }
  }

  if (findMatches.length > 0) {
    navigateFind(1, true);  // 跳转到第一个，静默
  } else {
    if (countEl) countEl.textContent = "No results";
  }
}

function navigateFind(direction, silent) {
  if (findMatches.length === 0) return;

  findIndex += direction;
  if (findIndex >= findMatches.length) findIndex = 0;
  if (findIndex < 0) findIndex = findMatches.length - 1;

  var rows = document.querySelectorAll("#graph-commits .gc-row");
  var currentEl = document.querySelector("#graph-commits .gc-row.gc-find-current");
  if (currentEl) currentEl.classList.remove("gc-find-current");

  var targetIdx = findMatches[findIndex];
  var targetRow = rows[targetIdx];
  if (targetRow) {
    targetRow.classList.add("gc-find-current");
    // 滚动到可见
    targetRow.scrollIntoView({ block: "center", behavior: "smooth" });
  }

  var countEl = document.getElementById("graph-find-count");
  if (countEl) {
    countEl.textContent = (findIndex + 1) + " of " + findMatches.length;
  }
}

// ---------- Canvas Tooltip ----------
function initCanvasTooltip() {
  var canvas = document.getElementById("graph-canvas");
  if (!canvas) return;

  // 创建 tooltip 元素
  graphTooltipEl = document.createElement("div");
  graphTooltipEl.id = "graph-tooltip";
  graphTooltipEl.style.display = "none";
  document.body.appendChild(graphTooltipEl);

  canvas.addEventListener("mousemove", function(e) {
    if (!currentGraphRef || graphCommits.length === 0) return;
    showCanvasTooltip(e);
  });

  canvas.addEventListener("mouseleave", function() {
    hideCanvasTooltip();
  });

  // 滚动时隐藏
  var body = document.querySelector(".graph-overlay-body");
  if (body) {
    body.addEventListener("scroll", function() {
      hideCanvasTooltip();
    });
  }
}

function showCanvasTooltip(e) {
  var canvas = document.getElementById("graph-canvas");
  var rect = canvas.getBoundingClientRect();
  var dpr = window.devicePixelRatio || 1;

  // 计算鼠标在 canvas 逻辑坐标中的位置
  var mx = (e.clientX - rect.left);
  var my = (e.clientY - rect.top);

  // 转换为网格坐标（考虑 dpr 缩放）
  var gridX = Math.floor((mx - LANE_W / 2) / LANE_W);
  var gridY = Math.floor(my / ROW_H);

  if (gridY < 0 || gridY >= graphCommits.length) {
    hideCanvasTooltip();
    return;
  }

  // 找到该位置最近的顶点
  var vertex = currentGraphRef.vertices[gridY];
  if (!vertex || vertex.isNotOnBranch()) {
    hideCanvasTooltip();
    return;
  }

  var commit = graphCommits[gridY];
  if (!commit || commit.uncommitted) {
    hideCanvasTooltip();
    return;
  }

  // 检查鼠标是否足够接近的顶点
  var vx = vertex.getPoint().x * LANE_W + LANE_W / 2;
  var vy = vertex.getPoint().y * ROW_H + ROW_H / 2;
  var dist = Math.sqrt((mx - vx) * (mx - vx) + (my - vy) * (my - vy));
  if (dist > DOT_R + 5) {
    hideCanvasTooltip();
    return;
  }

  // 构建 tooltip 内容
  var refs = parseRefs(commit.refs, knownRemotes);
  var branches = [], tags = [], remotes = [];
  for (var r = 0; r < refs.length; r++) {
    if (refs[r].type === "branch") branches.push(refs[r].label);
    else if (refs[r].type === "tag") tags.push(refs[r].label);
    else if (refs[r].type === "remote") remotes.push(refs[r].label);
  }

  var html = '<div class="gt-title">' + escapeHtml(commit.shortHash) + '</div>';
  html += '<div class="gt-subject">' + escapeHtml(commit.subject) + '</div>';
  html += '<div class="gt-meta">' + escapeHtml(commit.author) + ' · ' + escapeHtml(commit.date) + '</div>';
  if (branches.length > 0) {
    html += '<div class="gt-refs"><span class="gt-label">Branches: </span>' +
      branches.map(function(b) { return '<span class="gt-ref">' + escapeHtml(b) + '</span>'; }).join(" ") +
      '</div>';
  }
  if (tags.length > 0) {
    html += '<div class="gt-refs"><span class="gt-label">Tags: </span>' +
      tags.map(function(t) { return '<span class="gt-ref">' + escapeHtml(t) + '</span>'; }).join(" ") +
      '</div>';
  }
  if (remotes.length > 0) {
    html += '<div class="gt-refs"><span class="gt-label">Remotes: </span>' +
      remotes.map(function(r) { return '<span class="gt-ref">' + escapeHtml(r) + '</span>'; }).join(" ") +
      '</div>';
  }

  graphTooltipEl.innerHTML = html;
  graphTooltipEl.style.display = "block";

  // 定位：在鼠标右下方
  var tooltipX = e.clientX + 12;
  var tooltipY = e.clientY + 12;

  // 防止超出屏幕
  var tw = graphTooltipEl.offsetWidth || 200;
  var th = graphTooltipEl.offsetHeight || 60;
  if (tooltipX + tw > window.innerWidth - 10) tooltipX = e.clientX - tw - 12;
  if (tooltipY + th > window.innerHeight - 10) tooltipY = e.clientY - th - 12;

  graphTooltipEl.style.left = tooltipX + "px";
  graphTooltipEl.style.top = tooltipY + "px";
}

function hideCanvasTooltip() {
  if (graphTooltipEl) {
    graphTooltipEl.style.display = "none";
  }
}

// ---------- 初始化 ----------
function initGraph() {
  var resizeTimer;
  window.addEventListener("resize", function() {
    clearTimeout(resizeTimer);
    resizeTimer = setTimeout(handleResize, 150);
  });

  var observer = new ResizeObserver(function() {
    clearTimeout(resizeTimer);
    resizeTimer = setTimeout(handleResize, 100);
  });

  var graphContainer = document.getElementById("graph-container");
  if (graphContainer) {
    observer.observe(graphContainer);
  }

  initOverlayDrag();
  initOverlayResize();
  initFindWidget();
  initCanvasTooltip();

  // 对齐切换按钮
  var alignBtn = document.getElementById("graph-align-toggle");
  if (alignBtn) {
    alignBtn.addEventListener("click", toggleGraphAlign);
    if (alignedMode) alignBtn.classList.add("active");
  }

  // Fetch 按钮
  var fetchBtn = document.getElementById("graph-fetch-btn");
  if (fetchBtn) {
    fetchBtn.addEventListener("click", doFetch);
  }
}

// ---------- 对齐切换 ----------
function toggleGraphAlign() {
  alignedMode = !alignedMode;
  var btn = document.getElementById("graph-align-toggle");
  if (btn) {
    if (alignedMode) btn.classList.add("active"); else btn.classList.remove("active");
  }
  // 重绘
  var canvas = document.getElementById("graph-canvas");
  if (canvas && graphCommits.length > 0) {
    renderCommitList(graphCommits);
  }
}

// ---------- Overlay 拖拽 ----------
function initOverlayDrag() {
  var header = document.getElementById("graph-overlay-header");
  var panel = document.getElementById("graph-overlay-panel");
  if (!header || !panel) return;

  var isDragging = false;
  var startX, startY, startLeft, startTop;

  header.addEventListener("mousedown", function(e) {
    if (e.target.tagName === "BUTTON") return;
    isDragging = true;
    startX = e.clientX;
    startY = e.clientY;
    startLeft = panel.offsetLeft;
    startTop = panel.offsetTop;
    document.body.style.userSelect = "none";
  });

  document.addEventListener("mousemove", function(e) {
    if (!isDragging) return;
    var dx = e.clientX - startX;
    var dy = e.clientY - startY;
    panel.style.left = (startLeft + dx) + "px";
    panel.style.top = (startTop + dy) + "px";
  });

  document.addEventListener("mouseup", function() {
    if (isDragging) {
      isDragging = false;
      document.body.style.userSelect = "";
    }
  });
}

// ---------- Overlay 缩放 ----------
function initOverlayResize() {
  var handle = document.getElementById("graph-overlay-resize");
  var panel = document.getElementById("graph-overlay-panel");
  if (!handle || !panel) return;

  var isResizing = false;
  var startX, startY, startW, startH;

  handle.addEventListener("mousedown", function(e) {
    e.stopPropagation();
    isResizing = true;
    startX = e.clientX;
    startY = e.clientY;
    startW = panel.offsetWidth;
    startH = panel.offsetHeight;
    document.body.style.userSelect = "none";
  });

  document.addEventListener("mousemove", function(e) {
    if (!isResizing) return;
    var dw = e.clientX - startX;
    var dh = e.clientY - startY;
    panel.style.width = Math.max(480, startW + dw) + "px";
    panel.style.height = Math.max(320, startH + dh) + "px";
    panel.style.right = "auto";
    panel.style.bottom = "auto";
  });

  document.addEventListener("mouseup", function() {
    if (isResizing) {
      isResizing = false;
      document.body.style.userSelect = "";
      handleResize();
    }
  });
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", initGraph);
} else {
  initGraph();
}

})();
