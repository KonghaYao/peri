// ========== Git Graph 布局引擎 ==========
// 移植自 vscode-git-graph 的 core 算法

const NULL_VERTEX_ID = -1;
const LANE_COLORS = [
  "#00bcd4", "#ff9800", "#ab47bc", "#66bb6a",
  "#ff7043", "#42a5f5", "#ef5350", "#8d6e63",
  "#26a69a", "#ec407a", "#7e57c2", "#fdd835",
];

export const LANE_W = 16;
export const ROW_H = 28;
export const DOT_R = 4;
export const LINE_W = 2;

function Point(x, y) { this.x = x; this.y = y; }

function Branch(colour) {
  this.colour = colour;
  this.lines = [];
  this.end = 0;
}
Branch.prototype.addLine = function(p1, p2, lockedFirst) {
  this.lines.push({ p1, p2, lockedFirst });
};

function Vertex(id) {
  this.id = id;
  this.x = 0;
  this.children = [];
  this.parents = [];
  this.nextParent = 0;
  this.onBranch = null;
  this.isCurrent = false;
  this.nextX = 0;
  this.connections = [];
}
Vertex.prototype.addChild = function(v) { this.children.push(v); };
Vertex.prototype.addParent = function(v) { this.parents.push(v); };
Vertex.prototype.getNextParent = function() { return this.nextParent < this.parents.length ? this.parents[this.nextParent] : null; };
Vertex.prototype.registerParentProcessed = function() { this.nextParent++; };
Vertex.prototype.isMerge = function() { return this.parents.length > 1; };
Vertex.prototype.isNotOnBranch = function() { return this.onBranch === null; };
Vertex.prototype.isOnThisBranch = function(branch) { return this.onBranch === branch; };
Vertex.prototype.getBranch = function() { return this.onBranch; };
Vertex.prototype.addToBranch = function(branch, x) { if (this.onBranch === null) { this.onBranch = branch; this.x = x; } };
Vertex.prototype.getPoint = function() { return new Point(this.x, this.id); };
Vertex.prototype.getNextPoint = function() { return new Point(this.nextX, this.id); };
Vertex.prototype.getPointConnectingTo = function(vertex, onBranch) {
  for (let i = 0; i < this.connections.length; i++) {
    const conn = this.connections[i];
    if (conn && conn.connectsTo === vertex && conn.onBranch === onBranch) return new Point(i, this.id);
  }
  return null;
};
Vertex.prototype.registerUnavailablePoint = function(x, connectsToVertex, onBranch) {
  if (x === this.nextX) {
    this.nextX = x + 1;
    this.connections[x] = { connectsTo: connectsToVertex, onBranch };
  }
};

export function Graph(colours) {
  this.vertices = [];
  this.branches = [];
  this.availableColours = [];
  this.colours = colours || LANE_COLORS;
  this.activeHash = null;
}
Graph.prototype.loadCommits = function(commits) {
  this.vertices = [];
  this.branches = [];
  this.availableColours = [];
  if (commits.length === 0) return;

  for (let i = 0; i < commits.length; i++) this.vertices.push(new Vertex(i));

  const commitLookup = {};
  for (let i = 0; i < commits.length; i++) { if (commits[i].hash) commitLookup[commits[i].hash] = i; }

  const nullVertex = new Vertex(NULL_VERTEX_ID);
  for (let i = 0; i < commits.length; i++) {
    const parents = commits[i].parents || [];
    for (const p of parents) {
      const parentIdx = commitLookup[p];
      if (typeof parentIdx === 'number') {
        this.vertices[i].addParent(this.vertices[parentIdx]);
        this.vertices[parentIdx].addChild(this.vertices[i]);
      } else {
        this.vertices[i].addParent(nullVertex);
      }
    }
  }

  for (let i = 0; i < commits.length; i++) {
    if (commits[i].head) { this.vertices[i].isCurrent = true; break; }
  }

  let i = 0;
  while (i < this.vertices.length) {
    if (this.vertices[i].getNextParent() !== null || this.vertices[i].isNotOnBranch()) {
      this.determinePath(i);
    } else {
      i++;
    }
  }
};
Graph.prototype.determinePath = function(startAt) {
  let i = startAt;
  let vertex = this.vertices[i];
  let parentVertex = vertex.getNextParent();
  let curVertex, curPoint;
  let lastPoint = vertex.isNotOnBranch() ? vertex.getNextPoint() : vertex.getPoint();

  if (parentVertex !== null && parentVertex.id !== NULL_VERTEX_ID
      && vertex.isMerge() && !vertex.isNotOnBranch()
      && !parentVertex.isNotOnBranch()) {
    let foundPointToParent = false;
    const parentBranch = parentVertex.getBranch();
    for (i = startAt + 1; i < this.vertices.length; i++) {
      curVertex = this.vertices[i];
      curPoint = curVertex.getPointConnectingTo(parentVertex, parentBranch);
      if (curPoint !== null) foundPointToParent = true;
      else curPoint = curVertex.getNextPoint();
      const lockedFirst = !foundPointToParent && curVertex !== parentVertex ? lastPoint.x < curPoint.x : true;
      parentBranch.addLine(lastPoint, curPoint, lockedFirst);
      curVertex.registerUnavailablePoint(curPoint.x, parentVertex, parentBranch);
      lastPoint = curPoint;
      if (foundPointToParent) { vertex.registerParentProcessed(); break; }
    }
  } else {
    const branch = new Branch(this.getAvailableColour(startAt));
    vertex.addToBranch(branch, lastPoint.x);
    vertex.registerUnavailablePoint(lastPoint.x, vertex, branch);
    for (i = startAt + 1; i < this.vertices.length; i++) {
      curVertex = this.vertices[i];
      curPoint = (parentVertex === curVertex && !parentVertex.isNotOnBranch()) ? curVertex.getPoint() : curVertex.getNextPoint();
      branch.addLine(lastPoint, curPoint, lastPoint.x < curPoint.x);
      curVertex.registerUnavailablePoint(curPoint.x, parentVertex, branch);
      lastPoint = curPoint;
      if (parentVertex === curVertex) {
        vertex.registerParentProcessed();
        const parentWasOnBranch = !parentVertex.isNotOnBranch();
        parentVertex.addToBranch(branch, curPoint.x);
        vertex = parentVertex;
        parentVertex = vertex.getNextParent();
        if (parentVertex === null || parentWasOnBranch) break;
      }
    }
    if (i === this.vertices.length && parentVertex !== null && parentVertex.id === NULL_VERTEX_ID) {
      vertex.registerParentProcessed();
    }
    branch.end = i;
    this.branches.push(branch);
    this.availableColours[branch.colour] = i;
  }
};
Graph.prototype.getAvailableColour = function(startAt) {
  for (let i = 0; i < this.availableColours.length; i++) {
    if (startAt > this.availableColours[i]) return i;
  }
  this.availableColours.push(0);
  return this.availableColours.length - 1;
};
Graph.prototype.getContentWidth = function(laneW) {
  let x = 0;
  for (const v of this.vertices) { const p = v.getNextPoint(); if (p.x > x) x = p.x; }
  if (x < 2) x = 2;
  return (x + 1) * (laneW || LANE_W);
};

/** 在 canvas 上绘制整个 graph */
export function drawGraph(ctx, graph, commits, options = {}) {
  const { laneW = LANE_W, rowH = ROW_H, dotR = DOT_R, lineW = LINE_W } = options;
  const colours = graph.colours;
  const vertices = graph.vertices;
  const branches = graph.branches;

  // 分支线
  for (const branch of branches) {
    const colour = colours[branch.colour % colours.length];
    const lines = branch.lines;
    if (lines.length === 0) continue;

    ctx.strokeStyle = colour;
    ctx.lineWidth = lineW;
    ctx.lineCap = "round";
    ctx.lineJoin = "round";

    for (const line of lines) {
      const x1 = line.p1.x * laneW + laneW / 2;
      const y1 = line.p1.y * rowH + rowH / 2;
      const x2 = line.p2.x * laneW + laneW / 2;
      const y2 = line.p2.y * rowH + rowH / 2;

      const c1 = commits[line.p1.y] || {};
      const c2 = commits[line.p2.y] || {};
      if (c1.stash || c2.stash) {
        ctx.strokeStyle = "#9c27b0";
        ctx.setLineDash([3, 3]);
      } else if (c1.uncommitted || c2.uncommitted) {
        ctx.strokeStyle = "#808080";
        ctx.setLineDash([3, 3]);
      } else {
        ctx.strokeStyle = colour;
        ctx.setLineDash([]);
      }

      ctx.beginPath();
      if (x1 === x2) {
        ctx.moveTo(x1, y1);
        ctx.lineTo(x2, y2);
      } else {
        const d = rowH * 0.8;
        ctx.moveTo(x1, y1);
        ctx.bezierCurveTo(x1, y1 + d, x2, y2 - d, x2, y2);
      }
      ctx.stroke();
    }
  }
  ctx.setLineDash([]);

  // 节点
  for (const vertex of vertices) {
    if (vertex.isNotOnBranch()) continue;
    const commit = commits[vertex.id] || {};
    const colour = colours[vertex.getBranch().colour % colours.length];
    const cx = vertex.getPoint().x * laneW + laneW / 2;
    const cy = vertex.getPoint().y * rowH + rowH / 2;

    if (commit.uncommitted) {
      ctx.fillStyle = "#ffffff";
      ctx.beginPath(); ctx.arc(cx, cy, dotR + 1, 0, Math.PI * 2); ctx.fill();
      ctx.strokeStyle = "#808080"; ctx.lineWidth = lineW; ctx.setLineDash([2, 2]); ctx.stroke(); ctx.setLineDash([]);
    } else if (commit.stash) {
      ctx.fillStyle = "#9c27b0";
      const s = dotR + 1.5;
      ctx.beginPath(); ctx.moveTo(cx, cy - s); ctx.lineTo(cx + s, cy); ctx.lineTo(cx, cy + s); ctx.lineTo(cx - s, cy); ctx.closePath(); ctx.fill();
    } else {
      ctx.fillStyle = colour;
      ctx.beginPath(); ctx.arc(cx, cy, dotR, 0, Math.PI * 2); ctx.fill();
      ctx.strokeStyle = "#ffffff"; ctx.lineWidth = 2.5; ctx.stroke();
      if (vertex.isCurrent) { ctx.strokeStyle = colour; ctx.lineWidth = lineW; ctx.stroke(); }
    }
  }
}
