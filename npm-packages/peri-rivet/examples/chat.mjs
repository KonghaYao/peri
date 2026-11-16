/** 真实 wasm runtime 的多轮 Chat demo：node examples/chat.mjs（先按 README 的安装与运行构建本包）。 */
import { stdin, stdout } from 'node:process';
import { createInterface } from 'node:readline/promises';
import { BareHarnessConfig, ChatUserAction, TursoStorage } from '@peri-code/sdk';
import {
  createHostResourceOwner,
  createMemoryChatOwnership,
  createPeriChatActor,
  WorkspaceURI,
} from '@peri-code/rivet/rivet';
import { setup } from 'rivetkit';
import { createClient } from 'rivetkit/client';

const DEFAULT_MODEL = 'deepseek-v4-flash';
const DEFAULT_MACHINE_ID = '00000000-0000-4000-8000-000000000001';
const DEFAULT_ENGINE_PORT = 8961;
const INSTRUCTIONS = '你是 Peri。请用清晰、简洁的中文帮助用户完成任务。';

const apiKey = process.env.ANTHROPIC_AUTH_TOKEN ?? process.env.ANTHROPIC_API_KEY;
const baseUrl = process.env.ANTHROPIC_BASE_URL;
const storageUrl = process.env.PERI_STORAGE_URL;
const storageToken = process.env.PERI_STORAGE_TOKEN;

// 启动前一次性报齐缺项；缺任何一项都不降级，也不输出已有值。
const missing = [
  ...(apiKey ? [] : ['ANTHROPIC_AUTH_TOKEN 或 ANTHROPIC_API_KEY']),
  ...(baseUrl ? [] : ['ANTHROPIC_BASE_URL']),
  ...(storageUrl ? [] : ['PERI_STORAGE_URL']),
  ...(storageToken ? [] : ['PERI_STORAGE_TOKEN']),
];
if (missing.length > 0) {
  console.error('缺少环境变量，未启动：');
  for (const name of missing) {
    console.error(`  - ${name}`);
  }
  process.exit(1);
}

const model = process.env.ANTHROPIC_DEFAULT_SONNET_MODEL ?? DEFAULT_MODEL;
const machineId = process.env.PERI_MACHINE_ID ?? DEFAULT_MACHINE_ID;
const enginePort = Number(process.env.PERI_ENGINE_PORT ?? DEFAULT_ENGINE_PORT);
const chatId = `rt_chat_${crypto.randomUUID()}`;

// `agent.settings` 就是一份 PeriConfig；只包一层，WASM 从 `settings.config` 读 provider。
const periConfig = {
  config: {
    active_alias: 'sonnet',
    providers: [
      {
        id: 'anthropic',
        type: 'anthropic',
        apiKey,
        baseUrl,
        name: 'anthropic',
        models: { fable: model, opus: model, sonnet: model, haiku: model },
      },
    ],
    profiles: {
      fable: { provider: 'anthropic', model },
      opus: { provider: 'anthropic', model },
      sonnet: { provider: 'anthropic', model },
      haiku: { provider: 'anthropic', model },
    },
    meta_harness: {
      ...BareHarnessConfig,
      McpMiddleware: true,
      ToolSearch: true,
      WorkspaceMiddleware: false,
    },
  },
};

// 宿主强持有；actor 的 vars 被框架清掉后，未确认的 SDK/database 关闭仍归这里。
const owner = createHostResourceOwner();
const chat = createPeriChatActor({
  owner,
  openDatabase: (storage) => new TursoStorage(storage),
  ownership: createMemoryChatOwnership(),
  machineId,
  chatConfig: {
    runtime: { kind: 'wasm' },
    session: { instructions: INSTRUCTIONS },
  },
});

const registry = setup({
  use: { chat },
  startEngine: true,
  enginePort,
  startServices: false,
  shutdown: { disableSignalHandlers: true },
});
try {
  await registry.startAndWait();
} catch (error) {
  console.error(`engine failed: ${error instanceof Error ? error.name : typeof error}`);
  await registry.shutdown().catch(() => {});
  process.exit(1);
}

const client = createClient({
  endpoint: `http://127.0.0.1:${enginePort}`,
  disableMetadataLookup: true,
});
const handle = client.chat.getOrCreate([chatId]);
const connection = handle.connect();

let pendingTurn = null;
const detach = connection.on('chatFact', (envelope) => {
  if (envelope?.fact?.kind !== 'notice') {
    return;
  }
  const notification = envelope.fact.notification;
  if (notification.method === 'session/update') {
    const update = notification.params?.update;
    if (update?.sessionUpdate === 'agent_message_chunk' && update.content?.type === 'text') {
      stdout.write(update.content.text);
    }
    return;
  }
  if (notification.method === 'peri/agent_event_done' && pendingTurn !== null) {
    const finish = pendingTurn;
    pendingTurn = null;
    stdout.write('\n');
    finish(notification.params?.stopReason);
  }
});

let input = null;
let exitCode = 0;
try {
  await connection.ready;
  const opened = await handle.open({
    chatId,
    workspace: new WorkspaceURI({ machineId, runtimePath: process.cwd() }).toString(),
    agent: { settings: periConfig },
    storage: { url: storageUrl, authToken: storageToken },
  });
  console.log(`open    ${opened.chatId} (binding=${opened.binding === null ? 'null' : 'session'})`);
  console.log(`engine  http://127.0.0.1:${enginePort}`);
  console.log(`model   ${model}`);

  const created = await handle.execute(ChatUserAction.createNewSession({ requestId: `${chatId}:create` }));
  console.log(`session ${created.result.sessionId}`);
  console.log('输入 /exit 退出；Ctrl+C 也会清理进程。');

  input = createInterface({ input: stdin, output: stdout });
  input.on('SIGINT', () => {
    input?.close();
    // 等待模型时 Ctrl+C 也要结束当前 turn，否则循环卡在 finished 上。
    if (pendingTurn !== null) {
      const finish = pendingTurn;
      pendingTurn = null;
      finish('cancelled');
    }
  });
  for (;;) {
    let text;
    try {
      text = (await input.question('you> ')).trim();
    } catch {
      break; // Ctrl+C / EOF：输入已关闭，走统一清理。
    }
    if (text === '/exit') {
      break;
    }
    if (text.length === 0) {
      continue;
    }
    const finished = new Promise((resolve) => {
      pendingTurn = resolve;
    });
    stdout.write('assistant> ');
    await handle.execute(ChatUserAction.send({ text, requestId: `${chatId}:${crypto.randomUUID()}` }));
    const stopReason = await finished;
    if (stopReason !== 'end_turn') {
      console.log(`turn stopped: ${stopReason ?? 'unknown'}`);
    }
  }
} catch (error) {
  console.error(`chat failed: ${error instanceof Error ? error.name : typeof error}`);
  exitCode = 1;
} finally {
  input?.close();
  const releases = [
    // 先关闭新执行准入；closeAll 只处理仍由宿主持有的真实资源。
    () => owner.beginShutdown(),
    () => handle.close(),
    async () => detach(),
    () => connection.dispose(),
    () => client.dispose(),
    async () => {
      const report = await owner.closeAll({ timeoutMs: 5_000 });
      if (!report.complete) {
        console.error(`host resources incomplete: ${report.pending}`);
        exitCode = 1;
      }
    },
    // registry 与 client 收尾独立执行，不能因上一项失败被跳过。
    () => registry.shutdown(),
  ];
  for (const release of releases) {
    try {
      await release();
    } catch (error) {
      console.error(`cleanup failed: ${error instanceof Error ? error.name : typeof error}`);
      exitCode = 1;
    }
  }
}
process.exit(exitCode);
