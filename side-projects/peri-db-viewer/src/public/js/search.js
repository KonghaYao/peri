(function() { "use strict";

window.originalOnPageActivate5 = window.onPageActivate;
window.onPageActivate = function(pageName) {
  if (window.originalOnPageActivate5) window.originalOnPageActivate5(pageName);
  if (pageName === "search") { /* init handled by event listeners */ }
};

function escHtml(s) {
  if (!s) return "";
  return String(s).replace(/&/g,"&amp;").replace(/</g,"&lt;").replace(/>/g,"&gt;").replace(/"/g,"&quot;");
}

var searchPage = 1;
var searchPerPage = 20;

// ── 高亮关键词 ──
function highlightText(text, query) {
  if (!query || !text) return escHtml(text);
  var escaped = escHtml(text);
  // Escape regex special chars in query
  var safeQuery = query.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  var regex = new RegExp("(" + safeQuery + ")", "gi");
  return escaped.replace(regex, '<span class="search-highlight">$1</span>');
}

// ── 执行搜索 ──
async function doSearch() {
  var q = document.getElementById("search-input").value.trim();
  if (!q) return;
  var threadId = document.getElementById("search-thread-id").value.trim() || undefined;

  var params = new URLSearchParams({
    q: q, page: searchPage, perPage: searchPerPage,
  });
  if (threadId) params.set("thread_id", threadId);

  try {
    var data = await window.api.get("/api/search?" + params.toString());
    if (data.error) return;
    renderSearchResults(data.rows, q);
    renderSearchPagination(data.total, searchPage, searchPerPage);
  } catch (e) { console.error(e); }
}

function renderSearchResults(rows, query) {
  var tbody = document.getElementById("search-tbody");
  if (!rows || rows.length === 0) {
    tbody.innerHTML = '<tr><td colspan="3" class="empty-state">No results</td></tr>';
    return;
  }

  tbody.innerHTML = rows.map(function(r) {
    var contentPreview = "";
    try {
      var parsed = JSON.parse(r.content);
      if (parsed.content) {
        if (typeof parsed.content === "string") {
          contentPreview = parsed.content;
        } else if (Array.isArray(parsed.content)) {
          contentPreview = parsed.content.map(function(b) {
            return b.text || b.name || JSON.stringify(b);
          }).join(" | ");
        } else {
          contentPreview = JSON.stringify(parsed.content);
        }
      } else {
        contentPreview = r.content;
      }
    } catch (e) { contentPreview = r.content; }

    return '<tr>' +
      '<td><span class="thread-id-link" data-id="' + escHtml(r.thread_id) + '">' +
        escHtml(window.api.truncate(r.thread_title || r.thread_id, 40)) + '</span></td>' +
      '<td style="white-space:nowrap;"><span class="badge badge-muted">' + escHtml(r.role) + '</span></td>' +
      '<td>' + highlightText(window.api.truncate(contentPreview, 300), query) + '</td>' +
      '</tr>';
  }).join("");

  tbody.querySelectorAll(".thread-id-link").forEach(function(link) {
    link.addEventListener("click", function() {
      window.loadDetail(link.getAttribute("data-id"));
      document.querySelectorAll(".nav-tab").forEach(function(t) {
        t.classList.remove("active");
        if (t.getAttribute("data-page") === "detail") t.classList.add("active");
      });
      document.querySelectorAll(".page").forEach(function(p) { p.classList.remove("active"); });
      document.getElementById("page-detail").classList.add("active");
    });
  });
}

function renderSearchPagination(total, page, perPage) {
  var totalPages = Math.ceil(total / perPage);
  var el = document.getElementById("search-pagination");
  var html = "";
  html += '<button ' + (page <= 1 ? "disabled" : "") + ' data-page="' + (page-1) + '">Prev</button>';
  html += '<span style="padding:0 12px;font-size:13px;color:var(--text-muted)">' +
    page + ' / ' + totalPages + ' (' + total + ' results)</span>';
  html += '<button ' + (page >= totalPages ? "disabled" : "") + ' data-page="' + (page+1) + '">Next</button>';
  el.innerHTML = html;

  el.querySelectorAll("button").forEach(function(btn) {
    btn.addEventListener("click", function() {
      searchPage = parseInt(btn.getAttribute("data-page"));
      doSearch();
    });
  });
}

// ── Event Listeners ──
document.getElementById("search-btn").addEventListener("click", function() {
  searchPage = 1;
  doSearch();
});

document.getElementById("search-input").addEventListener("keydown", function(e) {
  if (e.key === "Enter") {
    searchPage = 1;
    doSearch();
  }
});

})();
