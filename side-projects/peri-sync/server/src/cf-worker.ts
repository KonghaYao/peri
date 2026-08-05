/// <reference types="@cloudflare/workers-types" />

import { PairRoom } from "./pair-do";
import { Channel } from "./channel-do";
import { CodeIndex } from "./code-index-do";
import { isValidChannelId } from "./canonical";
import { limits } from "./limits";

export { PairRoom, Channel, CodeIndex };

export interface WorkerEnv {
	PAIR_ROOM: DurableObjectNamespace;
	CHANNEL: DurableObjectNamespace;
	CODE_INDEX: DurableObjectNamespace;
	PERI_SYNC_PAYLOADS: R2Bucket;
	[key: string]: unknown;
}

type V1Route =
	| { kind: "create" }
	| { kind: "lookup" }
	| { kind: "channel"; id: string };

/**
 * v1 路由白名单（03-plan Slice 2）：其余路径/方法一律 404。
 * lookup 的码在 URL 路径中（服务端归一化；非法码与 miss 统一 404 无 oracle）。
 */
function matchV1(pathname: string, method: string): V1Route | null {
	if (method === "POST" && pathname === "/v1/channels") return { kind: "create" };
	if (method === "POST" && pathname.startsWith("/v1/code/")) return { kind: "lookup" };
	const ch = pathname.match(/^\/v1\/channels\/([^/]+)\/(join|code|parts|confirm|revoke)$/);
	if (method === "POST" && ch) return { kind: "channel", id: ch[1] };
	const hs = pathname.match(/^\/v1\/channels\/([^/]+)\/handshake\/(sender|receiver)$/);
	if (method === "POST" && hs) return { kind: "channel", id: hs[1] };
	const dl = pathname.match(/^\/v1\/channels\/([^/]+)\/parts\/(\d+)$/);
	if (method === "GET" && dl) return { kind: "channel", id: dl[1] };
	return null;
}

export default {
	async fetch(request: Request, env: WorkerEnv): Promise<Response> {
		const url = new URL(request.url);

		if (request.method === "GET" && url.pathname === "/health") {
			return new Response("ok");
		}

		if (url.pathname === "/ws") {
			// 旧 PairRoom WebSocket 中继：Slice 4 部署验证前保持不变。
			const id = env.PAIR_ROOM.idFromName("global");
			const stub = env.PAIR_ROOM.get(id);
			return stub.fetch(request);
		}

		const route = matchV1(url.pathname, request.method);
		if (!route) return new Response("Not Found", { status: 404 });

		// body 大小上限（POST）：读取后以重建 Request 转发，避免流重复消费。
		if (request.method === "POST") {
			const body = await request.arrayBuffer();
			if (body.byteLength > limits(env).maxBodyBytes) {
				return new Response(JSON.stringify({ error: "TOO_LARGE" }), {
					status: 413,
					headers: { "content-type": "application/json; charset=utf-8" },
				});
			}
			const headers = new Headers(request.headers);
			headers.delete("content-length");
			request = new Request(request.url, {
				method: request.method,
				headers,
				body: body.byteLength > 0 ? body : null,
			});
		}

		if (route.kind === "create") {
			// create 需全局 device:create 限流（per-channel DO 无法全局限流）：
			// 由 CodeIndex（全局单例）限流后转发 Channel DO。
			const ns = env.CODE_INDEX;
			const stub = ns.get(ns.idFromName("v1:index"));
			return stub.fetch(request);
		}
		if (route.kind === "lookup") {
			const ns = env.CODE_INDEX;
			const stub = ns.get(ns.idFromName("v1:index"));
			return stub.fetch(request);
		}

		if (!isValidChannelId(route.id)) {
			return new Response("Not Found", { status: 404 });
		}
		const ns = env.CHANNEL;
		const stub = ns.get(ns.idFromName(`v1:${route.id}`));
		return stub.fetch(request);
	},
};
