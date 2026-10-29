/**
 * 关闭 `@openuidev/react-lang` 的开发期自动挂载。
 *
 * 该包在模块求值时检查 `process.env.NODE_ENV === "development"`，并在下一
 * 微任务里动态 import `@openuidev/devtools`，把 Inspect 面板注入 `document.body`。
 * 我们不需要宿主页面里的调试面板，也不希望开发时多出一次 devtools 下载。
 *
 * 实现依赖包内使用的全局 symbol：`Symbol.for("openui.devtools.autoMount")`
 * 一旦被置为真值，自动挂载分支就不会执行。这是内部约定而非公开 API，
 * 升级 `@openuidev/react-lang` 时必须重新核对（当前锁定 1.0.2）。
 *
 * 必须在任何 `@openuidev/react-lang` import 之前完成求值：ESM 按 import
 * 声明顺序求值依赖，因此本模块要作为首个 import 出现在 openui 入口。
 */
const AUTO_MOUNT_FLAG = Symbol.for("openui.devtools.autoMount");

(globalThis as Record<symbol, unknown>)[AUTO_MOUNT_FLAG] = true;

export {};
