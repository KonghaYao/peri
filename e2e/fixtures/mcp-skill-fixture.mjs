// W6 A 线：临时 fixture MCP server（外部 stdio 对端），驱动真实 peri 的
// workspace-resources 验收场景。
//
// 与仓库既有 wire 夹具同形态（`peri-acp/src/host/mcp_v4_wire_fixture_test.rs`
// 的 `WIRE_FIXTURE_SCRIPT` / `peri-middlewares/tests/mcp_isolation_contract.rs`）：
// **每条**收到的 JSON-RPC 行先落自己的 wire 日志（`#recv <payload>`），
// 解析日志即「wire 事实」——不依赖任何 mock 计数（TEST-EVIDENCE-001）。
//
// 能力面（SEP-2640 规范路径）：
// - `capabilities.extensions["io.modelcontextprotocol/skills"]`：声明技能扩展，
//   使客户端走 `skills/list`（而非 legacy `resources/list` 扫描）；
// - `skills/list` / `skills/get`：返回一条技能条目（frontmatter + 完整
//   resources[] 含 sha256 digest）；
// - `resources/list`：返回一条 Agent 资源（`agent://remote/{name}/agent.md`）；
// - `resources/read`：按 URI 返回技能正文 / Agent 定义正文（digest 自洽；
//   未知 URI → -32602）；
// - `server/discover` 一律 -32601（驱动客户端 Auto 回退 legacy `initialize`，
//   与既有夹具逐字同构）。
//
// 环境变量（由测试注入，全部为隔离临时路径，不读真实用户数据）：
// - `FX_LOG`：wire 日志路径（必填）；
// - `FX_NAME`：server 名（默认 `skillfx`，注入 peri 配置的 mcpServers 键）；
// - `FX_SKILL`：技能名（默认 `demo-skill`）；
// - `FX_AGENT`：远端 Agent 名（默认 `beta`）。
//
// 正文哨兵经 env 可改写（`FX_SKILL_BODY` / `FX_AGENT_BODY`），测试用固定默认值。
import { createHash } from "node:crypto";
import { appendFileSync } from "node:fs";
import readline from "node:readline";

const logPath = process.env.FX_LOG;
const serverName = process.env.FX_NAME || "skillfx";
const skillName = process.env.FX_SKILL || "demo-skill";
const agentName = process.env.FX_AGENT || "beta";
const log = (line) => {
  if (logPath) appendFileSync(logPath, `${line}\n`);
};

const skillBody =
  process.env.FX_SKILL_BODY ||
  `---
name: ${skillName}
description: W6-FX-SKILL-DESC
---

# Fixture skill
W6-FX-SKILL-BODY-SENTINEL
`;

const agentBody =
  process.env.FX_AGENT_BODY ||
  `---
name: ${agentName}
description: W6-FX-AGENT-DESC
---

You are the fixture remote agent ${agentName}. Reply with FX-AGENT-REPLY-OK.
`;

const skillUri = `skill://${serverName}/${skillName}/SKILL.md`;
const agentUri = `agent://remote/${agentName}/agent.md`;
const skillEntry = {
  uri: skillUri,
  frontmatter: { name: skillName, description: "W6-FX-SKILL-DESC" },
  resources: [
    {
      uri: skillUri,
      digest: `sha256:${createHash("sha256").update(skillBody, "utf8").digest("hex")}`,
    },
  ],
};

log(`#boot ${serverName} pid=${process.pid}`);

const rl = readline.createInterface({ input: process.stdin });
rl.on("line", (line) => {
  log(`#recv ${line}`);
  let request;
  try {
    request = JSON.parse(line);
  } catch {
    return;
  }
  if (request.id === undefined) return;
  const reply = (result) =>
    process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", id: request.id, result })}\n`);
  const refuse = (code, message) =>
    process.stdout.write(
      `${JSON.stringify({ jsonrpc: "2.0", id: request.id, error: { code, message } })}\n`,
    );
  switch (request.method) {
    case "initialize":
      reply({
        protocolVersion: "2025-11-25",
        capabilities: {
          tools: {},
          resources: {},
          extensions: { "io.modelcontextprotocol/skills": {} },
        },
        serverInfo: { name: serverName, version: "1.0.0-fx" },
      });
      break;
    case "tools/list":
      reply({ tools: [] });
      break;
    case "resources/list":
      reply({
        resources: [
          {
            uri: agentUri,
            name: agentName,
            mimeType: "text/markdown",
            description: "W6-FX-AGENT-DESC",
          },
        ],
      });
      break;
    case "skills/list":
      reply({ skills: [skillEntry] });
      break;
    case "skills/get":
      reply({ skill: skillEntry });
      break;
    case "resources/read": {
      const uri = request.params?.uri ?? "";
      if (uri === skillUri) {
        reply({ contents: [{ uri, mimeType: "text/markdown", text: skillBody }] });
      } else if (uri === agentUri) {
        reply({ contents: [{ uri, mimeType: "text/markdown", text: agentBody }] });
      } else {
        refuse(-32602, `unknown resource: ${uri}`);
      }
      break;
    }
    case "ping":
      reply({});
      break;
    default:
      refuse(-32601, "Method not found");
  }
});
