// ========== 文件预览组件 ==========

// 初始化 mermaid
mermaid.initialize({
  startOnLoad: false,
  theme: "neutral",
  themeVariables: {
    primaryColor: "#0969da",
    primaryTextColor: "#1f2328",
    primaryBorderColor: "#0969da",
    lineColor: "#656d76",
    secondaryColor: "#f6f8fa",
    tertiaryColor: "#f0f2f5",
  },
  flowchart: { useMaxWidth: true, htmlLabels: true },
  sequence: { useMaxWidth: true },
  gantt: { useMaxWidth: true },
});

const preview = document.getElementById("preview");

// ---------- 格式化文件大小 ----------
function fmtSize(bytes) {
  if (bytes < 1024) return bytes + " B";
  if (bytes < 1024 * 1024) return (bytes / 1024).toFixed(1) + " KB";
  return (bytes / (1024 * 1024)).toFixed(1) + " MB";
}

// ---------- 获取语言名称 ----------
function getLangName(lang) {
  const names = {
    markdown: "Markdown", javascript: "JavaScript", typescript: "TypeScript",
    jsx: "React JSX", tsx: "React TSX", json: "JSON", html: "HTML",
    css: "CSS", python: "Python", ruby: "Ruby", go: "Go",
    rust: "Rust", java: "Java", c: "C", cpp: "C++",
    bash: "Bash", sql: "SQL", xml: "XML", yaml: "YAML",
    toml: "TOML", graphql: "GraphQL", scss: "SCSS", less: "Less",
  };
  return names[lang] || lang;
}

// ---------- 文件信息栏 ----------
function infoBar(name, size, path) {
  return '<div class="file-info-bar">' +
    '<span class="file-name">' + escapeHtml(name) + '</span>' +
    '<div style="display:flex;align-items:center;gap:8px;">' +
      (size !== undefined ? '<span class="file-size">' + fmtSize(size) + '</span>' : "") +
      '<button class="btn-refresh" title="刷新预览" data-refresh="' + escapeHtml(path || '') + '">⟳</button>' +
    '</div>' +
    '</div>';
}

// ---------- 解码 HTML 实体 ----------
function decodeHtmlEntities(text) {
  const el = document.createElement("textarea");
  el.innerHTML = text;
  return el.value;
}

// ---------- HTML 转义 ----------
function escapeHtml(text) {
  const el = document.createElement("div");
  el.textContent = text;
  return el.innerHTML;
}

// ---------- 代码高亮 ----------
function highlightCode(code, lang) {
  if (lang && hljs.getLanguage(lang)) {
    try {
      const result = hljs.highlight(code, { language: lang });
      return result.value;
    } catch (e) {
      // fallback
    }
  }
  return escapeHtml(code);
}

// ---------- 渲染 mermaid 图表 ----------
async function renderMermaidBlocks(html) {
  const mermaidRegex = /<pre><code class="language-mermaid">([\s\S]*?)<\/code><\/pre>/g;
  const matches = [...html.matchAll(mermaidRegex)];

  if (matches.length === 0) return html;

  const promises = matches.map(async function(match, idx) {
    const code = decodeHtmlEntities(match[1].trim());
    try {
      const mermaidId = "mermaid-" + Date.now() + "-" + idx;
      const result = await mermaid.render(mermaidId, code);
      return {
        original: match[0],
        replacement: '<div class="mermaid-container">' + result.svg + '</div>',
      };
    } catch (err) {
      return {
        original: match[0],
        replacement: '<div class="mermaid-container" style="color:#f38ba8;padding:20px;">' +
          '<strong>Mermaid 渲染失败:</strong>' +
          '<pre style="margin-top:8px;white-space:pre-wrap;font-size:12px;">' +
          escapeHtml(err.message || String(err)) + '\n\n' + escapeHtml(code) + '</pre></div>',
      };
    }
  });

  const results = await Promise.all(promises);
  let result = html;
  for (const r of results) {
    result = result.replace(r.original, r.replacement);
  }
  return result;
}

// ---------- 渲染 PlantUML 图表 ----------
async function renderPlantUmlBlocks(html) {
  var pumlRegex = /<pre><code class="language-plantuml">([\s\S]*?)<\/code><\/pre>/g;
  var matches = [...html.matchAll(pumlRegex)];

  // 也匹配 language-puml
  var pumlRegex2 = /<pre><code class="language-puml">([\s\S]*?)<\/code><\/pre>/g;
  matches = matches.concat([...html.matchAll(pumlRegex2)]);

  if (matches.length === 0) return html;

  var promises = matches.map(async function(match, idx) {
    var code = decodeHtmlEntities(match[1].trim());
    try {
      var resp = await fetch("https://kroki.io/plantuml/svg", {
        method: "POST",
        body: code,
        headers: { "Content-Type": "text/plain" },
      });
      if (!resp.ok) throw new Error("HTTP " + resp.status);
      var svg = await resp.text();
      // 移除固定宽高，由 CSS 控制自适应
      svg = svg.replace(/<svg([^>]*)>/, function(m, attrs) {
        attrs = attrs.replace(/\s*width="[^"]*"/g, "");
        attrs = attrs.replace(/\s*height="[^"]*"/g, "");
        return "<svg" + attrs + ">";
      });
      return {
        original: match[0],
        replacement: '<div class="plantuml-container">' +
          '<button class="btn-fullscreen" title="全屏查看" onclick="toggleFullscreen(this)">⛶</button>' +
          svg + '</div>',
      };
    } catch (err) {
      return {
        original: match[0],
        replacement: '<div class="plantuml-container" style="color:#cf222e;padding:20px;">' +
          '<strong>PlantUML 渲染失败:</strong>' +
          '<pre style="margin-top:8px;white-space:pre-wrap;font-size:12px;">' +
          escapeHtml(err.message || String(err)) + '\n\n' + escapeHtml(code) + '</pre></div>',
      };
    }
  });

  var results = await Promise.all(promises);
  var result = html;
  for (var i = 0; i < results.length; i++) {
    result = result.replace(results[i].original, results[i].replacement);
  }
  return result;
}

// ---------- 渲染 Markdown ----------
async function renderMarkdown(md) {
  // 用 marked 渲染
  let html = marked.parse(md);

  // 手动对代码块做高亮
  html = html.replace(
    /<pre><code class="language-(\w+)">([\s\S]*?)<\/code><\/pre>/g,
    function(_, lang, code) {
      const decoded = decodeHtmlEntities(code);
      if (lang === "mermaid" || lang === "plantuml" || lang === "puml") {
        // 留到后面分别处理
        return '<pre><code class="language-' + lang + '">' + escapeHtml(decoded) + '</code></pre>';
      }
      const highlighted = highlightCode(decoded, lang);
      return '<pre><code class="language-' + lang + ' hljs">' + highlighted + '</code></pre>';
    }
  );

  // 无语言的代码块
  html = html.replace(
    /<pre><code>([\s\S]*?)<\/code><\/pre>/g,
    function(_, code) {
      const decoded = decodeHtmlEntities(code);
      return '<pre><code>' + escapeHtml(decoded) + '</code></pre>';
    }
  );

  // 处理 mermaid
  html = await renderMermaidBlocks(html);

  // 处理 plantuml
  html = await renderPlantUmlBlocks(html);

  return html;
}

// ---------- 主预览入口 ----------
async function renderPreview(path, name) {
  preview.innerHTML = '<div class="loading"><div class="spinner"></div></div>';

  const ext = name.includes(".") ? name.split(".").pop().toLowerCase() : "";

  // 图片文件
  const imageExts = ["png", "jpg", "jpeg", "gif", "webp", "svg", "ico", "bmp"];
  if (imageExts.includes(ext)) {
    preview.innerHTML =
      infoBar(name, undefined, path) +
      '<div class="image-preview">' +
      '<img src="/api/file?path=' + encodeURIComponent(path) + '" alt="' + escapeHtml(name) + '">' +
      '</div>';
    return;
  }

  // 其他二进制文件
  const binaryExts = ["pdf", "zip", "tar", "gz", "woff", "woff2", "ttf", "eot", "mp4", "mp3", "mov", "avi"];
  if (binaryExts.includes(ext)) {
    preview.innerHTML =
      '<div class="unsupported-preview">' +
      '<div class="icon">📦</div>' +
      '<div>' + escapeHtml(name) + '</div>' +
      '<div style="font-size:14px;color:var(--text-muted)">二进制文件，无法预览</div>' +
      '<a href="/api/file?path=' + encodeURIComponent(path) + '" download style="color:var(--accent);margin-top:8px;">⬇ 下载</a>' +
      '</div>';
    return;
  }

  // 文本文件：先查缓存
  try {
    var cacheKeyName = "file:" + path;
    var cached = cacheGet(cacheKeyName);

    if (cached) {
      renderTextPreview(cached, path, name, ext);
      // 后台刷新缓存
      fetch("/api/file?path=" + encodeURIComponent(path))
        .then(function(resp) { return resp.json(); })
        .then(function(data) {
          if (!data.error) cacheSet(cacheKeyName, data);
        })
        .catch(function() {});
      return;
    }

    const resp = await fetch("/api/file?path=" + encodeURIComponent(path));
    const data = await resp.json();

    if (data.error) {
      preview.innerHTML =
        '<div class="error-preview">' +
        '<h3>⚠ 读取失败</h3>' +
        '<pre>' + escapeHtml(data.error) + '</pre>' +
        '</div>';
      return;
    }

    // 存入缓存
    cacheSet(cacheKeyName, data);
    renderTextPreview(data, path, name, ext);

  } catch (err) {
    preview.innerHTML =
      '<div class="error-preview">' +
      '<h3>⚠ 请求失败</h3>' +
      '<pre>' + escapeHtml(err.message || String(err)) + '</pre>' +
      '</div>';
  }
}

// ---------- 文本预览渲染（从缓存或 API 数据）----------
async function renderTextPreview(data, path, name, ext) {
    var content = data.content || "";
    var lang = data.language || "";

    // Markdown
    if (ext === "md") {
      var mdHtml = await renderMarkdown(content);
      preview.innerHTML = infoBar(name, data.size, path) + '<div class="md-body">' + mdHtml + '</div>';
      return;
    }

    // HTML 文件
    if (ext === "html" || ext === "htm") {
      preview.innerHTML = infoBar(name, data.size, path) +
        '<div class="code-preview"><pre><code class="language-html hljs">' +
        highlightCode(content, "html") + '</code></pre></div>';
      return;
    }

    // 其他代码/文本文件
    preview.innerHTML =
      infoBar(name, data.size, path) +
      '<div class="code-preview">' +
      '<pre><code class="language-' + lang + ' hljs">' +
      highlightCode(content, lang) + '</code></pre>' +
      '</div>';
}

// ---------- 刷新按钮事件委托 ----------
document.getElementById("preview").addEventListener("click", function(e) {
  var btn = e.target.closest(".btn-refresh");
  if (!btn) return;

  var filePath = btn.dataset.refresh;
  if (!filePath) return;

  var name = filePath.split("/").pop();

  // 舍弃缓存，强制重新请求
  localStorage.removeItem(cacheKey("file:" + filePath));
  localStorage.removeItem(cacheKey("tree:" + filePath));

  renderPreview(filePath, name);

  // 如果全屏开着，等渲染完后更新全屏内容
  var fullscreenOverlay = document.querySelector(".fullscreen-overlay");
  if (fullscreenOverlay && fullscreenOverlay.dataset.file === filePath) {
    setTimeout(function() {
      var newContainer = document.querySelector(".plantuml-container");
      var newSvg = newContainer ? newContainer.querySelector("svg") : null;
      var body = fullscreenOverlay.querySelector(".fullscreen-body");
      if (newSvg && body) {
        body.innerHTML = "";
        body.appendChild(newSvg.cloneNode(true));
      }
    }, 500);
  }
  // 旋转动画
  btn.style.transform = "rotate(360deg)";
  btn.style.transition = "transform 0.3s ease";
  setTimeout(function() {
    btn.style.transform = "";
    btn.style.transition = "";
  }, 300);
});

// ---------- PlantUML 全屏 ----------
function toggleFullscreen(btn) {
  var container = btn.closest(".plantuml-container");
  if (!container) return;

  var existing = document.querySelector(".fullscreen-overlay");
  if (existing) {
    existing.remove();
    return;
  }

  var svg = container.querySelector("svg");
  if (!svg) return;

  var overlay = document.createElement("div");
  overlay.className = "fullscreen-overlay";
  // 记录文件路径，供刷新时同步更新全屏
  var refreshBtn = document.querySelector(".btn-refresh");
  overlay.dataset.file = refreshBtn ? refreshBtn.dataset.refresh : "";

  var toolbar = document.createElement("div");
  toolbar.className = "fullscreen-toolbar";
  var closeBtn = document.createElement("button");
  closeBtn.textContent = "✕";
  closeBtn.title = "退出全屏";
  closeBtn.onclick = function() { overlay.remove(); };
  toolbar.appendChild(closeBtn);
  overlay.appendChild(toolbar);

  var body = document.createElement("div");
  body.className = "fullscreen-body";
  body.appendChild(svg.cloneNode(true));
  overlay.appendChild(body);

  overlay.addEventListener("click", function(e) {
    if (e.target === overlay) overlay.remove();
  });

  document.addEventListener("keydown", function escHandler(e) {
    if (e.key === "Escape") {
      overlay.remove();
      document.removeEventListener("keydown", escHandler);
    }
  });

  document.body.appendChild(overlay);
}
