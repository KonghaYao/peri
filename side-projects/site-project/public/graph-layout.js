// ========== Git Graph 布局引擎 ==========
// 移植自 vscode-git-graph 的 web/graph.ts 核心算法
// 职责：给定 commit 列表（含 parents 关系），计算每个 commit 的 lane 位置和分支线条
(function() {

var NULL_VERTEX_ID = -1;

// ---------- Point ----------
function Point(x, y) {
  this.x = x;
  this.y = y;
}

// ---------- Branch ----------
function Branch(colour) {
  this.colour = colour;
  this.lines = [];  // {p1: Point, p2: Point, lockedFirst: boolean}[]
  this.end = 0;
}

Branch.prototype.addLine = function(p1, p2, lockedFirst) {
  this.lines.push({ p1: p1, p2: p2, lockedFirst: lockedFirst });
};

// ---------- Vertex ----------
function Vertex(id) {
  this.id = id;
  this.x = 0;
  this.children = [];
  this.parents = [];
  this.nextParent = 0;      // 当前正在处理的父节点索引
  this.onBranch = null;     // Branch | null
  this.isCurrent = false;
  this.nextX = 0;           // 下一个可用的 lane 位置
  this.connections = [];    // {connectsTo: Vertex|null, onBranch: Branch}[]
}

Vertex.prototype.addChild = function(v) {
  this.children.push(v);
};

Vertex.prototype.addParent = function(v) {
  this.parents.push(v);
};

Vertex.prototype.getNextParent = function() {
  if (this.nextParent < this.parents.length) return this.parents[this.nextParent];
  return null;
};

Vertex.prototype.registerParentProcessed = function() {
  this.nextParent++;
};

Vertex.prototype.isMerge = function() {
  return this.parents.length > 1;
};

Vertex.prototype.isNotOnBranch = function() {
  return this.onBranch === null;
};

Vertex.prototype.isOnThisBranch = function(branch) {
  return this.onBranch === branch;
};

Vertex.prototype.getBranch = function() {
  return this.onBranch;
};

Vertex.prototype.addToBranch = function(branch, x) {
  if (this.onBranch === null) {
    this.onBranch = branch;
    this.x = x;
  }
};

Vertex.prototype.getPoint = function() {
  return new Point(this.x, this.id);
};

Vertex.prototype.getNextPoint = function() {
  return new Point(this.nextX, this.id);
};

// 查找是否已有连接点到指定顶点
Vertex.prototype.getPointConnectingTo = function(vertex, onBranch) {
  for (var i = 0; i < this.connections.length; i++) {
    var conn = this.connections[i];
    if (conn && conn.connectsTo === vertex && conn.onBranch === onBranch) {
      return new Point(i, this.id);
    }
  }
  return null;
};

// 标记某个 lane 位置已被占用
Vertex.prototype.registerUnavailablePoint = function(x, connectsToVertex, onBranch) {
  if (x === this.nextX) {
    this.nextX = x + 1;
    this.connections[x] = { connectsTo: connectsToVertex, onBranch: onBranch };
  }
};

// ---------- Graph ----------
function Graph(colours) {
  this.vertices = [];
  this.branches = [];
  this.availableColours = [];  // 每个颜色最后使用的顶点结束位置
  this.colours = colours;
}

// 加载 commit 数据，计算 lane 布局
Graph.prototype.loadCommits = function(commits) {
  this.vertices = [];
  this.branches = [];
  this.availableColours = [];

  if (commits.length === 0) return;

  // 1. 创建所有顶点
  for (var i = 0; i < commits.length; i++) {
    this.vertices.push(new Vertex(i));
  }

  // 2. 建立 hash → index 映射
  var commitLookup = {};
  for (var i = 0; i < commits.length; i++) {
    if (commits[i].hash) commitLookup[commits[i].hash] = i;
  }

  // 3. 建立 parent-child 关系
  var nullVertex = new Vertex(NULL_VERTEX_ID);
  for (var i = 0; i < commits.length; i++) {
    var parents = commits[i].parents;
    for (var j = 0; j < parents.length; j++) {
      var parentIdx = commitLookup[parents[j]];
      if (typeof parentIdx === 'number') {
        this.vertices[i].addParent(this.vertices[parentIdx]);
        this.vertices[parentIdx].addChild(this.vertices[i]);
      } else {
        // 父节点不在当前加载范围内（超出 -n 200 限制）
        this.vertices[i].addParent(nullVertex);
      }
    }
  }

  // 4. 标记 HEAD commit
  for (var i = 0; i < commits.length; i++) {
    if (commits[i].head) {
      this.vertices[i].isCurrent = true;
      break;
    }
  }

  // 5. 主循环：为每个顶点确定路径
  var i = 0;
  while (i < this.vertices.length) {
    if (this.vertices[i].getNextParent() !== null || this.vertices[i].isNotOnBranch()) {
      this.determinePath(i);
    } else {
      i++;
    }
  }
};

// 核心算法：从一个顶点开始，沿父链向下分配 lane 和分支
Graph.prototype.determinePath = function(startAt) {
  var i = startAt;
  var vertex = this.vertices[i];
  var parentVertex = vertex.getNextParent();
  var curVertex, curPoint;

  var lastPoint = vertex.isNotOnBranch() ? vertex.getNextPoint() : vertex.getPoint();

  if (parentVertex !== null && parentVertex.id !== NULL_VERTEX_ID
      && vertex.isMerge() && !vertex.isNotOnBranch()
      && !parentVertex.isNotOnBranch()) {
    // === Merge 情况：两个已有分支之间的连接 ===
    var foundPointToParent = false;
    var parentBranch = parentVertex.getBranch();

    for (i = startAt + 1; i < this.vertices.length; i++) {
      curVertex = this.vertices[i];
      curPoint = curVertex.getPointConnectingTo(parentVertex, parentBranch);

      if (curPoint !== null) {
        foundPointToParent = true;
      } else {
        curPoint = curVertex.getNextPoint();
      }

      var lockedFirst = !foundPointToParent && curVertex !== parentVertex
        ? lastPoint.x < curPoint.x
        : true;
      parentBranch.addLine(lastPoint, curPoint, lockedFirst);
      curVertex.registerUnavailablePoint(curPoint.x, parentVertex, parentBranch);
      lastPoint = curPoint;

      if (foundPointToParent) {
        vertex.registerParentProcessed();
        break;
      }
    }
  } else {
    // === 普通情况：创建新分支，沿父链向下走 ===
    var branch = new Branch(this.getAvailableColour(startAt));
    vertex.addToBranch(branch, lastPoint.x);
    vertex.registerUnavailablePoint(lastPoint.x, vertex, branch);

    for (i = startAt + 1; i < this.vertices.length; i++) {
      curVertex = this.vertices[i];
      curPoint = (parentVertex === curVertex && !parentVertex.isNotOnBranch())
        ? curVertex.getPoint()
        : curVertex.getNextPoint();

      branch.addLine(lastPoint, curPoint, lastPoint.x < curPoint.x);
      curVertex.registerUnavailablePoint(curPoint.x, parentVertex, branch);
      lastPoint = curPoint;

      if (parentVertex === curVertex) {
        // 到达父节点，继续沿父链走
        vertex.registerParentProcessed();
        var parentWasOnBranch = !parentVertex.isNotOnBranch();
        parentVertex.addToBranch(branch, curPoint.x);
        vertex = parentVertex;
        parentVertex = vertex.getNextParent();

        if (parentVertex === null || parentWasOnBranch) {
          break;
        }
      }
    }

    // 如果是列表最后一个顶点且父节点超界
    if (i === this.vertices.length && parentVertex !== null && parentVertex.id === NULL_VERTEX_ID) {
      vertex.registerParentProcessed();
    }

    branch.end = i;
    this.branches.push(branch);
    this.availableColours[branch.colour] = i;
  }
};

// 颜色复用：找第一个已结束的分支颜色
Graph.prototype.getAvailableColour = function(startAt) {
  for (var i = 0; i < this.availableColours.length; i++) {
    if (startAt > this.availableColours[i]) {
      return i;
    }
  }
  this.availableColours.push(0);
  return this.availableColours.length - 1;
};

// 获取内容宽度（像素），根据最大 lane 数
Graph.prototype.getContentWidth = function(laneW) {
  var x = 0;
  for (var i = 0; i < this.vertices.length; i++) {
    var p = this.vertices[i].getNextPoint();
    if (p.x > x) x = p.x;
  }
  // 至少 2 列
  if (x < 2) x = 2;
  return (x + 1) * laneW;
};

// 获取每个顶点处的图形宽度
Graph.prototype.getWidthsAtVertices = function(laneW) {
  var widths = [];
  for (var i = 0; i < this.vertices.length; i++) {
    widths[i] = this.vertices[i].getNextPoint().x * laneW;
  }
  return widths;
};

// 获取每个顶点的分支颜色索引（用于 data-color 色带）
Graph.prototype.getVertexColours = function() {
  var colours = [];
  for (var i = 0; i < this.vertices.length; i++) {
    var v = this.vertices[i];
    if (v.onBranch) {
      colours[i] = v.onBranch.colour;
    } else {
      colours[i] = -1;
    }
  }
  return colours;
};

// 获取每个顶点的 lane 位置（用于文本对齐）
Graph.prototype.getVertexXPositions = function() {
  var positions = [];
  for (var i = 0; i < this.vertices.length; i++) {
    var v = this.vertices[i];
    positions[i] = v.onBranch ? v.x : 0;
  }
  return positions;
};

// 暴露到全局
window.GitGraphLayout = {
  Branch: Branch,
  Vertex: Vertex,
  Graph: Graph,
  Point: Point,
  LANE_W: 16,
  ROW_H: 28,
  DOT_R: 4,
  LINE_W: 2,
  LANE_COLORS: [
    "#00bcd4", "#ff9800", "#ab47bc", "#66bb6a",
    "#ff7043", "#42a5f5", "#ef5350", "#8d6e63",
    "#26a69a", "#ec407a", "#7e57c2", "#fdd835",
  ],
};

})();
