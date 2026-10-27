import type { Server } from 'node:net';

export async function pickPort(server: Server, host: string, preferredPort: number, fallback = false): Promise<number> {
  const listen = (port: number) =>
    new Promise<number>((resolve, reject) => {
      const onError = (error: Error) => {
        server.off('listening', onListening);
        reject(error);
      };
      const onListening = () => {
        server.off('error', onError);
        const address = server.address();
        if (!address || typeof address === 'string') {
          reject(new Error('no TCP address after listening'));
          return;
        }
        resolve(address.port);
      };
      server.once('error', onError);
      server.once('listening', onListening);
      server.listen(port, host);
    });
  try {
    return await listen(preferredPort);
  } catch (error) {
    if (!fallback || (error as NodeJS.ErrnoException).code !== 'EADDRINUSE') throw error;
    return listen(0);
  }
}
