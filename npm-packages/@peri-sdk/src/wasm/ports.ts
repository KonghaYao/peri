import { promises as dns } from "node:dns";
import * as net from "node:net";
import { isIP } from "node:net";

/**
 * Host ports for the Emscripten ACP Host. Each port owns one piece of platform state
 * (timers, sockets, name resolution) and is injected into the module factory by `startPeriWasmHost`.
 */

export interface WasmSchedulerPort {
    schedule(callback: () => void, delayMs?: number): unknown;
    cancel(timer: unknown): void;
    /** Stop accepting callbacks and drop every pending timer owned by the host. */
    close(): void;
}

export interface WasmNetworkPort {
    /** Module-shaped `node:net` replacement handed to the Emscripten glue. */
    module: Record<string, unknown>;
    /** Destroy every socket this host still owns; throws when destruction is unconfirmed. */
    close(): void;
    /** Resolve once every destroyed socket reported its close event. */
    drain(): Promise<void>;
}

export interface WasmDnsAddress {
    address: string;
    family: 4 | 6;
}

export interface WasmDnsLookupOptions {
    family?: number;
    all?: boolean;
}

export interface WasmDnsPort {
    lookup(hostname: string, options: WasmDnsLookupOptions,
        callback: (error: Error | null, addresses?: WasmDnsAddress[]) => void): void;
}

/** Timers stay owned by the host so a closed host cannot run callbacks afterwards. */
export function createNodeSchedulerPort(): WasmSchedulerPort {
    const timers = new Set<ReturnType<typeof setTimeout>>();
    let closed = false;
    return {
        schedule(callback: () => void, delayMs = 0): ReturnType<typeof setTimeout> {
            if (closed) throw new Error("WASM scheduler is closed");
            const timer = setTimeout(() => {
                timers.delete(timer);
                if (!closed) callback();
            }, delayMs);
            timers.add(timer);
            return timer;
        },
        cancel(timer: unknown): void {
            clearTimeout(timer as ReturnType<typeof setTimeout>);
            timers.delete(timer as ReturnType<typeof setTimeout>);
        },
        close(): void {
            closed = true;
            for (const timer of timers) clearTimeout(timer);
            timers.clear();
        },
    };
}

/** Sockets stay owned by the host so shutdown closes only the connections it created. */
export function createNodeNetworkPort(): WasmNetworkPort {
    const sockets = new Set<net.Socket>();
    const closing = new Map<net.Socket, Promise<void>>();
    let closed = false;
    class HostSocket extends net.Socket {
        constructor(options?: net.SocketConstructorOpts) {
            super(options);
            if (closed) {
                this.destroy();
                throw new Error("WASM network is closed");
            }
            sockets.add(this);
            closing.set(this, new Promise<void>((resolve) => {
                this.once("close", () => {
                    sockets.delete(this);
                    closing.delete(this);
                    resolve();
                });
            }));
        }
    }
    return {
        module: { ...net, Socket: HostSocket },
        close(): void {
            closed = true;
            const errors: unknown[] = [];
            for (const socket of sockets) {
                try {
                    socket.destroy();
                    if (!socket.destroyed) throw new Error("WASM socket destruction is unconfirmed");
                    sockets.delete(socket);
                } catch (error) { errors.push(error); }
            }
            if (errors.length) throw new AggregateError(errors, "WASM network cleanup failed");
        },
        async drain(): Promise<void> {
            if (!closed) throw new Error("WASM network cleanup has not started");
            await Promise.all(closing.values());
        },
    };
}

export interface WasmDnsPortOptions {
    /** Host mappings that replace name resolution for the listed hostnames. */
    overrides?: Readonly<Record<string, readonly WasmDnsAddress[]>>;
    resolver?: Pick<typeof dns, "resolve4" | "resolve6">;
}

/** Record lookups replace the A/AAAA set only; the lookup contract itself stays host-defined. */
export function createNodeDnsPort(options: WasmDnsPortOptions = {}): WasmDnsPort {
    const hosts = options.overrides ?? {};
    const resolver = options.resolver ?? dns;
    for (const [hostname, addresses] of Object.entries(hosts))
        if (!hostname || !addresses.length || addresses.some(entry => isIP(entry.address) !== entry.family))
            throw new Error("Invalid WASM DNS override");
    return {
        lookup(hostname: string, options: WasmDnsLookupOptions,
            callback: (error: Error | null, addresses?: WasmDnsAddress[]) => void): void {
            const family: 4 | 6 = options.family === 6 ? 6 : 4;
            const override = hosts[hostname];
            const resolving: Promise<WasmDnsAddress[]> = override
                ? Promise.resolve(override.filter(entry => entry.family === family).map(entry => ({ ...entry })))
                : family === 6
                    ? resolver.resolve6(hostname).then(addresses => addresses.map(address => ({ address, family })))
                    : resolver.resolve4(hostname).then(addresses => addresses.map(address => ({ address, family })));
            resolving.then(addresses => callback(null, addresses), error => callback(error));
        },
    };
}
