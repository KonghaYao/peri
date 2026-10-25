import { fileURLToPath } from "node:url";

/** 本地开发：Bun 服务（watch）与 Vite 前端同时启动；Vite 把 `/api` 代理到服务端口。 */
const packageRoot = fileURLToPath(new URL("../", import.meta.url));
const vite = fileURLToPath(new URL("../node_modules/.bin/vite", import.meta.url));

const children = [
  Bun.spawn([
    "bun", "--watch", "--tsconfig-override", "server/tsconfig.json", "server/index.ts",
  ], {
    cwd: packageRoot, stdin: "inherit", stdout: "inherit", stderr: "inherit",
  }),
  Bun.spawn([vite], {
    cwd: packageRoot, stdin: "inherit", stdout: "inherit", stderr: "inherit",
  }),
];

const stop = () => { for (const child of children) child.kill(); };
process.once("SIGINT", stop);
process.once("SIGTERM", stop);

const first = await Promise.race(children.map((child) => child.exited));
stop();
const codes = await Promise.all(children.map((child) => child.exited));
process.exitCode = first === 0 ? Math.max(...codes) : first;
