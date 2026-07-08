(function() { "use strict";

window.originalOnPageActivate3 = window.onPageActivate;
window.onPageActivate = function(pageName) {
  if (window.originalOnPageActivate3) window.originalOnPageActivate3(pageName);
  // Detail page is activated via loadDetail, not nav tab
};

function escHtml(s) {
  if (!s) return "";
  return String(s).replace(/&/g,"&amp;").replace(/</g,"&lt;").replace(/>/g,"&gt;").replace(/"/g,"&quot;");
}

window.loadDetail = async function(threadId) {
  var metaEl = document.getElementById("detail-meta");
  var relEl = document.getElementById("detail-relations");
  var msgEl = document.getElementById("detail-messages");

  metaEl.innerHTML = '<div class="empty-state">Loading...</div>';
  relEl.innerHTML = "";
  msgEl.innerHTML = "";

  try {
    var data = await window.api.get("/api/threads/" + encodeURIComponent(threadId));
    if (data.error) { metaEl.innerHTML = '<div class="empty-state">Error: ' + escHtml(data.error) + '</div>'; return; }
    renderMeta(data.thread);
    renderRelations(data.thread, data.parent, data.children);
  } catch (e) {
    metaEl.innerHTML = '<div class="empty-state">Failed to load</div>';
    return;
  }

  try {
    var msgData = await window.api.get("/api/threads/" + encodeURIComponent(threadId) + "/messages");
    if (msgData.error) return;
    renderMessages(msgData.messages, threadId);
  } catch (e) { console.error(e); }
};

// ── 渲染元数据卡片 ──
function renderMeta(thread) {
  var el = document.getElementById("detail-meta");
  var items = [
    { label: "ID", value: thread.id },
    { label: "Title", value: thread.title || "(untitled)" },
    { label: "Status", value: thread.agent_status || "-" },
    { label: "CWD", value: thread.cwd || "-" },
    { label: "Messages", value: String(thread.message_count) },
    { label: "Created", value: window.api.formatDate(thread.created_at) },
    { label: "Updated", value: window.api.formatDate(thread.updated_at) },
    { label: "Cancel Policy", value: thread.cancel_policy || "-" },
  ];
  el.innerHTML = '<div class="detail-meta-grid">' + items.map(function(i) {
    return '<div class="meta-item"><div class="meta-label">' + escHtml(i.label) +
      '</div><div class="meta-value">' + escHtml(i.value) + '</div></div>';
  }).join("") + '</div>';
}

// ── 渲染关系链 ──
function renderRelations(thread, parent, children) {
  var el = document.getElementById("detail-relations");
  var parts = [];

  if (parent) {
    parts.push('<span class="relation-link" data-id="' + escHtml(parent.id) + '">Parent: ' +
      escHtml(parent.id.substring(0, 8)) + '...</span>');
    parts.push('<span class="relation-arrow">&rarr;</span>');
  }
  parts.push('<strong>' + escHtml(thread.id.substring(0, 12)) + '...</strong>');
  if (children && children.length > 0) {
    parts.push('<span class="relation-arrow">&rarr;</span>');
    parts.push('<span>' + children.length + ' sub-agent(s)</span>');
  }

  if (parts.length > 1) {
    el.innerHTML = '<div class="relation-chain">' + parts.join(" ") + '</div>';

    if (parent) {
      el.querySelector(".relation-link").addEventListener("click", function() {
        window.loadDetail(parent.id);
      });
    }
  }

  // Sub-agents table
  if (children && children.length > 0) {
    el.innerHTML += '<div class="sub-agents-section"><h3>Sub-Agents (' + children.length + ')</h3>' +
      '<div class="table-wrapper"><table><thead><tr><th>ID</th><th>Title</th><th>Status</th><th>Messages</th><th>Created</th></tr></thead><tbody>' +
      children.map(function(c) {
        return '<tr><td><span class="thread-id-link" data-id="' + escHtml(c.id) + '">' +
          escHtml(c.id.substring(0, 8)) + '...</span></td>' +
          '<td>' + escHtml(window.api.truncate(c.title || "(untitled)", 50)) + '</td>' +
          '<td><span class="badge badge-info">' + escHtml(c.agent_status || "-") + '</span></td>' +
          '<td>' + c.message_count + '</td>' +
          '<td>' + window.api.formatDate(c.created_at) + '</td></tr>';
      }).join("") +
      '</tbody></table></div></div>';

    el.querySelectorAll(".thread-id-link").forEach(function(link) {
      link.addEventListener("click", function() {
        window.loadDetail(link.getAttribute("data-id"));
      });
    });
  }
}

// ── 渲染消息时间线 ──
function renderMessages(messages, threadId) {
  var el = document.getElementById("detail-messages");

  if (!messages || messages.length === 0) {
    el.innerHTML = '<div class="empty-state">No messages</div>';
    return;
  }

  // 倒序显示（最新在前）
  messages = messages.slice().reverse();

  // Pass 1: 构建 tool_call_id → tool_name 映射
  var toolNameMap = {};
  messages.forEach(function(m) {
    var p = null;
    try { p = JSON.parse(m.content); } catch (e) {}
    if (p && p.role === "assistant" && Array.isArray(p.content)) {
      p.content.forEach(function(block) {
        if (block.type === "tool_use" && block.id && block.name) {
          toolNameMap[block.id] = block.name;
        }
      });
    }
  });

  el.innerHTML = messages.map(function(m, idx) {
    var parsed = null;
    try { parsed = JSON.parse(m.content); } catch (e) {}

    if (parsed && parsed.role === "tool") {
      // 独立 tool 消息（role: "tool"）
      return renderToolMessage(parsed, idx, toolNameMap, messages.length);
    }

    var roleClass = "message-role role-" + m.role;
    var blocksHtml = "";

    if (parsed && Array.isArray(parsed.content)) {
      blocksHtml = parsed.content.map(function(block) {
        switch (block.type) {
          case "text":
            return renderTextBlock(block);
          case "tool_use":
            return renderToolUseBlock(block);
          case "tool_result":
            return renderToolResultBlock(block, toolNameMap);
          case "reasoning":
          case "thinking":
            return renderReasoningBlock(block);
          default:
            return renderUnknownBlock(block);
        }
      }).join("");
    } else if (parsed && typeof parsed.content === "string") {
      // user/system 等消息的 content 是纯文本
      blocksHtml = '<div class="message-content-block block-text">' +
        escHtml(window.api.truncate(parsed.content, 2000)) + '</div>';
    } else {
      blocksHtml = '<div class="message-raw">' + escHtml(
        typeof m.content === "string" ? window.api.truncate(m.content, 2000) : JSON.stringify(m.content).substring(0, 2000)
      ) + '</div>';
    }

    return '<div class="message-item">' +
      '<div class="' + roleClass + '">' + escHtml(m.role) + ' #' + (messages.length - idx) +
      ' <span class="message-id-label">' + escHtml(m.message_id ? m.message_id.substring(0, 8)+"..." : "") + '</span>' +
      '</div>' +
      blocksHtml +
      '</div>';
  }).join("");
}

// ── 独立 tool 消息 ──
function renderToolMessage(parsed, idx, toolNameMap, total) {
  var toolCallId = parsed.tool_call_id || "-";
  var toolName = toolNameMap[toolCallId] || "";
  var isError = !!parsed.is_error;
  var resultContent = typeof parsed.content === "string" ? parsed.content : JSON.stringify(parsed.content || "");
  var truncated = window.api.truncate(resultContent, 800);

  var badgeHtml = isError
    ? '<span class="badge badge-error tool-result-badge">ERROR</span>'
    : '<span class="badge badge-done tool-result-badge">OK</span>';

  return '<div class="message-item tool-message' + (isError ? ' tool-error' : '') + '">' +
    '<div class="message-role role-tool">tool #' + (total - idx) + '</div>' +
    '<div class="tool-result-header">' +
      '<span class="tool-result-icon">' + (isError ? '&#10007;' : '&#10003;') + '</span>' +
      (toolName ? '<span class="tool-name-badge">' + escHtml(toolName) + '</span>' : '') +
      '<span class="tool-call-id">' + escHtml(toolCallId) + '</span>' +
      badgeHtml +
    '</div>' +
    '<div class="message-content-block block-tool-result' + (isError ? ' is-error' : '') + '">' +
      escHtml(truncated) +
    '</div>' +
    '</div>';
}

// ── Text block ──
function renderTextBlock(block) {
  return '<div class="message-content-block block-text">' +
    escHtml(window.api.truncate(block.text || "", 2000)) + '</div>';
}

// ── Tool Use block ──
function renderToolUseBlock(block) {
  var inputEntries = block.input ? Object.entries(block.input) : [];
  var paramsHtml = "";
  if (inputEntries.length > 0) {
    var shown = inputEntries.slice(0, 5);
    paramsHtml = '<div class="tool-params">' +
      shown.map(function(entry) {
        var val = String(entry[1]);
        var preview = val.length > 100 ? val.substring(0, 100) + "..." : val;
        // 转义 value 再放进来
        var escapedVal = preview.replace(/&/g,"&amp;").replace(/</g,"&lt;").replace(/>/g,"&gt;").replace(/"/g,"&quot;");
        return '<div class="tool-param-row"><span class="tool-param-key">' +
          escHtml(entry[0]) + '</span><span class="tool-param-val">' + escapedVal + '</span></div>';
      }).join("") +
      (inputEntries.length > 5
        ? '<div class="tool-param-more">+ ' + (inputEntries.length - 5) + ' more params</div>'
        : '') +
      '</div>';
  }

  var callId = block.id ? escHtml(block.id.substring(0, 8) + "...") : "-";

  return '<div class="message-content-block block-tool-use">' +
    '<div class="tool-use-header">' +
      '<span class="tool-icon">&#9881;</span>' +
      '<strong class="tool-use-name">' + escHtml(block.name) + '</strong>' +
      '<span class="tool-use-id">' + callId + '</span>' +
    '</div>' +
    paramsHtml +
    '</div>';
}

// ── Tool Result block (embedded in assistant message) ──
function renderToolResultBlock(block, toolNameMap) {
  var errClass = block.is_error ? " is-error" : "";
  var toolName = toolNameMap[block.tool_use_id] || "";
  var resultPreview = escHtml(window.api.truncate(
    typeof block.content === "string" ? block.content : JSON.stringify(block.content), 500
  ));

  return '<div class="message-content-block block-tool-result' + errClass + '">' +
    (toolName ? '<span class="tool-name-badge">' + escHtml(toolName) + ' &rarr;</span> ' : '') +
    (block.is_error ? '<span class="badge badge-error">ERROR</span> ' : '') +
    '<span class="tool-call-id">' + escHtml(block.tool_use_id || "-") + '</span>' +
    '<br>' + resultPreview +
    '</div>';
}

// ── Reasoning block ──
function renderReasoningBlock(block) {
  return '<div class="message-content-block block-reasoning">' +
    escHtml(window.api.truncate(block.text || "", 1000)) + '</div>';
}

// ── Unknown block ──
function renderUnknownBlock(block) {
  return '<div class="message-content-block block-text" style="opacity:0.6;">' +
    '[' + escHtml(block.type) + '] ' + escHtml(JSON.stringify(block).substring(0, 200)) + '</div>';
}

})();
