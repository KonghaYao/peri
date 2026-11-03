import {
  closeSync,
  constants,
  existsSync,
  fchmodSync,
  fstatSync,
  lstatSync,
  mkdirSync,
  openSync,
  renameSync,
  unlinkSync,
  writeSync,
} from 'node:fs';
import { join } from 'node:path';

/** Bounded rotating stderr log under `<sandboxRoot>/diagnostics`. */
export class DiagnosticLog {
  private readonly directory: string;
  private descriptor: number;
  private bytes: number;
  private closed = false;
  constructor(
    sandboxRoot: string,
    private readonly maxBytes = 10 * 1024 * 1024,
  ) {
    if (maxBytes < 1) throw new Error('invalid diagnostic log configuration');
    const root = join(sandboxRoot, 'diagnostics');
    const rootEntry = lstatSync(root, { throwIfNoEntry: false });
    if (rootEntry && (rootEntry.isSymbolicLink() || !rootEntry.isDirectory()))
      throw new Error('diagnostic root is not a real directory');
    this.directory = root;
    mkdirSync(this.directory, { recursive: true, mode: 0o700 });
    const stats = lstatSync(this.directory);
    if (!stats.isDirectory() || stats.isSymbolicLink() || stats.mode & 0o077)
      throw new Error('diagnostic directory must be private and real');
    this.descriptor = this.open();
    this.bytes = fstatSync(this.descriptor).size;
  }
  private file(index = 0): string {
    return join(this.directory, `diagnostic.log${index ? `.${index}` : ''}`);
  }
  private open(): number {
    const descriptor = openSync(
      this.file(),
      constants.O_WRONLY | constants.O_APPEND | constants.O_CREAT | constants.O_NOFOLLOW,
      0o600,
    );
    if (!fstatSync(descriptor).isFile()) {
      closeSync(descriptor);
      throw new Error('diagnostic target is not a regular file');
    }
    fchmodSync(descriptor, 0o600);
    return descriptor;
  }
  private rotate(): void {
    closeSync(this.descriptor);
    if (existsSync(this.file(2))) unlinkSync(this.file(2));
    for (let index = 1; index >= 0; index--)
      if (existsSync(this.file(index))) renameSync(this.file(index), this.file(index + 1));
    this.descriptor = this.open();
    this.bytes = 0;
  }
  write(chunk: Buffer): void {
    if (this.closed) return;
    let offset = 0;
    while (offset < chunk.length) {
      if (this.bytes >= this.maxBytes) this.rotate();
      const length = Math.min(chunk.length - offset, this.maxBytes - this.bytes);
      const written = writeSync(this.descriptor, chunk, offset, length);
      if (!written) throw new Error('diagnostic log write made no progress');
      this.bytes += written;
      offset += written;
    }
  }
  close(): void {
    if (!this.closed) {
      this.closed = true;
      closeSync(this.descriptor);
    }
  }
}
