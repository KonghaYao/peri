import { registerHttp, registerMcp } from "./src/middleware.ts";
import { registerPrompts } from "./src/prompts.ts";
import { registerResources } from "./src/resources.ts";
import { server } from "./src/mcp-server.ts";
import { registerBasicsTools } from "./src/tools/basics.ts";
import { registerHostTools } from "./src/tools/host.ts";
import { registerMrtrTools } from "./src/tools/mrtr.ts";
import { registerNotesTools } from "./src/tools/notes.ts";
import { registerStateTools } from "./src/tools/state.ts";
import { registerTasksTools } from "./src/tools/tasks.ts";
import { registerTestingTools } from "./src/tools/testing.ts";

/**
 * 组合根：只负责按顺序装配能力。
 *
 * 顺序有观察意义：中间件必须先于工具挂载，tools/list 与 prompts/list 的顺序
 * 由这里的注册顺序决定。状态实现见 src/state.ts 与 src/notes.ts。
 */

registerHttp(server);
registerMcp(server);

registerBasicsTools(server);
registerNotesTools(server);
registerStateTools(server);
registerTasksTools(server);
registerMrtrTools(server);
registerHostTools(server);
registerTestingTools(server);

registerResources(server);
registerPrompts(server);

export default server;
