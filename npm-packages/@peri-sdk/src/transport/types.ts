/** ACP JSON-RPC frames over a host-injected transport. */
export type JsonRpcNotification = {
  jsonrpc: "2.0";
  method: string;
  params?: unknown;
};

export type ReverseRequestHandler = (
  method: string,
  params: unknown,
) => unknown | Promise<unknown>;

export interface Transport {
  request<T = unknown>(method: string, params?: unknown): Promise<T>;
  /** Resolves once the request is written; response settles independently. */
  sendRequest<T = unknown>(
    method: string,
    params?: unknown,
  ): Promise<{ response: Promise<T> }>;
  notify(method: string, params?: unknown): Promise<void>;
  subscribe(listener: (notification: JsonRpcNotification) => void): () => void;
  events(): AsyncIterable<JsonRpcNotification>;
  setRequestHandler(handler: ReverseRequestHandler): void;
  close(): Promise<void>;
}
