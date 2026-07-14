// ========== localStorage 缓存工具 ==========

var CACHE_PREFIX = "site-project:";
var CACHE_TTL = 30 * 60 * 1000; // 30 分钟过期

function cacheKey(key) {
  return CACHE_PREFIX + key;
}

function cacheGet(key) {
  try {
    var raw = localStorage.getItem(cacheKey(key));
    if (!raw) return null;
    var data = JSON.parse(raw);
    if (Date.now() - data.ts > CACHE_TTL) {
      localStorage.removeItem(cacheKey(key));
      return null;
    }
    return data.value;
  } catch (e) {
    return null;
  }
}

function cacheSet(key, value) {
  try {
    localStorage.setItem(cacheKey(key), JSON.stringify({
      value: value,
      ts: Date.now()
    }));
  } catch (e) {
    // localStorage 满或不可用
  }
}

function cacheClear() {
  var keys = Object.keys(localStorage);
  for (var i = 0; i < keys.length; i++) {
    if (keys[i].startsWith(CACHE_PREFIX)) {
      localStorage.removeItem(keys[i]);
    }
  }
}
