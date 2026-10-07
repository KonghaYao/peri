import { mkdir } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { createInterface } from "node:readline";
import type { AdmissionRequest, SettlementRequest } from "./admission-service";
import { localHostIdentity, LocalInstanceProofProvider } from "./local-proof";
import { SessionExecution } from "./session-execution";
import type { ActivationSource } from "./types";
import type { Transport } from "../transport/types";
import { RpcError } from "../transport/rpc-error";

function argument(name: string): string {
    const index = process.argv.indexOf(name);
    const value = index < 0 ? undefined : process.argv[index + 1];
    if (!value || value.startsWith("--")) throw new TypeError(`Missing ${name}`);
    return value;
}
const database = resolve(argument("--database"));
await mkdir(dirname(database), { recursive: true });
const replies = new Map<string, { resolve(value: unknown): void; reject(error: Error): void }>();
function request<Result>(method: string, params?: unknown): Promise<Result> {
    return new Promise((resolve, reject) => {
        const id = `sdk-${crypto.randomUUID()}`;
        replies.set(id, { resolve: resolve as (value: unknown) => void, reject });
        process.stdout.write(`${JSON.stringify({ id, method, params })}\n`);
    });
}
const transport: Transport = {
    request,
    sendRequest: async <Result>(method: string, params?: unknown) => ({ response: request<Result>(method, params) }),
    notify: async (method, params) => { process.stdout.write(`${JSON.stringify({ method, params })}\n`); },
    subscribe: () => () => {},
    events: async function* () {},
    setRequestHandler: () => {},
    close: async () => {},
};
const service = new SessionExecution(transport, { database, instance: {
    instanceId: argument("--instance-id"), generationId: argument("--generation-id"),
    proofRoute: { kind: "stdioSupervisor", reference: "trustedLocalSqliteAdmissionPort", hostIdentity: localHostIdentity(),
        dispatcherPid: process.pid, periPid: process.ppid },
}, proofProvider: new LocalInstanceProofProvider() });
const input = createInterface({ input: process.stdin, crlfDelay: Infinity });
const pending = new Set<Promise<void>>();
async function handle(line: string): Promise<void> {
    let id: unknown = null;
    try {
        if (Buffer.byteLength(line) > 64 * 1024 * 1024) throw new TypeError("Admission frame exceeds limit");
        const frame = JSON.parse(line);
        id = frame.id;
        if (typeof id === "string" && !frame.method && replies.has(id)) {
            const reply = replies.get(id)!;
            replies.delete(id);
            if (frame.error) reply.reject(new RpcError(frame.error.code, frame.error.message, frame.error.data));
            else if ("result" in frame) reply.resolve(frame.result);
            else reply.reject(new RpcError(-32603, "Malformed dispatcher reverse response"));
            return;
        }
        if (typeof id !== "string" || !id || typeof frame.method !== "string") throw new TypeError("Invalid JSONL request");
        let result: unknown;
        switch (frame.method) {
            case "peri/execution/ready": result = { protocolVersion: 1, durability: "durable" }; break;
            case "peri/execution/admit": result = await service.handle(frame.method, frame.params as AdmissionRequest); break;
            case "peri/execution/settle": result = await service.handle(frame.method, frame.params as SettlementRequest); break;
            case "peri/execution/entered": result = await service.handle(frame.method, frame.params); break;
            case "peri/execution/query": result = await service.query(frame.params.sessionId); break;
            case "peri/execution/activate": result = await service.activate(frame.params.sessionId, frame.params.source as ActivationSource ?? "notification"); break;
            default: throw new TypeError("Unknown execution method");
        }
        process.stdout.write(`${JSON.stringify({ id, result })}\n`);
    } catch (error) {
        process.stdout.write(`${JSON.stringify({ id, error: { code: -32602, message: error instanceof Error ? error.message : String(error) } })}\n`);
    }
}
for await (const line of input) {
    const operation = handle(line);
    pending.add(operation);
    void operation.finally(() => pending.delete(operation));
}
for (const reply of replies.values()) reply.reject(new RpcError(-32010, "Dispatcher ACP bridge disconnected; execution outcome unknown"));
replies.clear();
await Promise.all(pending);
service.close();
