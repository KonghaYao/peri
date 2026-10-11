import type { Defer } from "./types";

/** 浏览器没有请求生命周期：清理观察默认 fire-and-forget，错误已在观察链内记录。 */
export const defaultDefer: Defer = (promise) => { void promise.catch(() => {}); };
