import * as net from "node:net";

export function workerNetwork() {
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
