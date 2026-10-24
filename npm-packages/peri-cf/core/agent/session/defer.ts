import type { Defer } from "../types";

/** 清理观察默认 fire-and-forget，错误已在观察链内记录。 */
export const defaultDefer: Defer = (promise) => { void promise.catch(() => {}); };
