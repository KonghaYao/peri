import { chmod, readFile, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { localDnsOverrides } from "./local-dns";
import { localPreviewCommand } from "./local-preview-command";

const packageRoot = fileURLToPath(new URL("../", import.meta.url));
const sourceVars = new URL("../.dev.vars", import.meta.url);
const builtVars = new URL("../dist/peri_cf/.dev.vars", import.meta.url);
const overrides = await localDnsOverrides(sourceVars);
if (overrides) {
  const source = (await readFile(builtVars, "utf8")).replace(/^PERI_DNS_OVERRIDES=.*\n?/gm, "");
  await writeFile(builtVars, `${source.trimEnd()}\nPERI_DNS_OVERRIDES='${overrides}'\n`, { mode: 0o600 });
  await chmod(builtVars, 0o600);
}
const child = Bun.spawn(localPreviewCommand(packageRoot, process.argv.slice(2)), {
  cwd: packageRoot, stdin: "inherit", stdout: "inherit", stderr: "inherit",
});
for (const signal of ["SIGINT", "SIGTERM"] as const) process.once(signal, () => child.kill(signal));
process.exitCode = await child.exited;
