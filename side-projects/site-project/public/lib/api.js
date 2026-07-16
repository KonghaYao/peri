// ========== fetch 工具 ==========
// 所有 iframe 直连后端用。统一错误处理。

/**
 * GET JSON。
 * @param {string} url
 * @returns {Promise<any>}
 */
export async function getJSON(url) {
  const r = await fetch(url);
  if (!r.ok) throw new Error(`GET ${url} failed: ${r.status}`);
  try {
    return await r.json();
  } catch (e) {
    throw new Error(`GET ${url}: response is not valid JSON`);
  }
}

/**
 * POST/PATCH JSON。
 * @param {string} url
 * @param {string} method - 'POST' | 'PATCH' | 'DELETE'
 * @param {any} [body]
 * @returns {Promise<any>}
 */
export async function sendJSON(url, method = 'POST', body = undefined) {
  const r = await fetch(url, {
    method,
    headers: body !== undefined ? { 'Content-Type': 'application/json' } : undefined,
    body: body !== undefined ? JSON.stringify(body) : undefined,
  });
  if (!r.ok) throw new Error(`${method} ${url} failed: ${r.status}`);
  try {
    return await r.json();
  } catch (e) {
    throw new Error(`${method} ${url}: response is not valid JSON`);
  }
}

/** debounce 工具 */
export function debounce(fn, ms = 500) {
  let t = null;
  return (...args) => {
    if (t) clearTimeout(t);
    t = setTimeout(() => fn(...args), ms);
  };
}
