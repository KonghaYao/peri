import { createConnection } from 'node:net';
import { randomUUID } from 'node:crypto';
import type { SandboxControl, SandboxRecord } from './types.js';
import { ControlError } from './types.js';

export function connectSandboxControl({ socketPath }: { socketPath: string }): SandboxControl {
  const call = <T>(message: Record<string, unknown>): Promise<T> =>
    new Promise((resolve, reject) => {
      const socket = createConnection(socketPath);
      let input = '';
      socket.setTimeout(30000, () => socket.destroy(new Error('control timeout')));
      socket.on('error', reject);
      socket.on('connect', () => socket.write(JSON.stringify(message) + '\n'));
      socket.on('data', (data) => {
        input += data;
        if (input.length > 1048576) {
          socket.destroy(new Error('large response'));
          return;
        }
        if (input.includes('\n')) {
          try {
            const reply = JSON.parse(input.slice(0, input.indexOf('\n')));
            socket.end();
            if (reply.error) reject(new ControlError(reply.error.code, reply.error.message));
            else resolve(reply.result);
          } catch (error) {
            socket.destroy();
            reject(error);
          }
        }
      });
      socket.on('close', () => reject(new ControlError('interrupted', 'control connection closed before response')));
    });
  return {
    create: (spec, { requestId }) => call<SandboxRecord>({ method: 'create', spec, requestId }),
    start: (id, { requestId }) => call<SandboxRecord>({ method: 'start', id, requestId }),
    stop: (id, { requestId }) => call<SandboxRecord>({ method: 'stop', id, requestId }),
    destroy: (id, { requestId, deleteData }) => call<SandboxRecord>({ method: 'destroy', id, requestId, deleteData }),
    inspect: (id) => call<SandboxRecord>({ method: 'inspect', id }),
    list: () => call<SandboxRecord[]>({ method: 'list' }),
  };
}
export const newRequestId = (): string => randomUUID();
export type { SandboxControl, SandboxRecord };
