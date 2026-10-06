import { readFileSync, readlinkSync } from "node:fs";
import { hostname } from "node:os";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import type { InstanceDescriptor, InstanceProofProvider } from "./types";

export function localHostIdentity(): string {
    let machine: string;
    if (process.platform === "linux") machine = [readFileSync("/etc/machine-id", "utf8").trim(),
        readFileSync("/proc/sys/kernel/random/boot_id", "utf8").trim(), readlinkSync("/proc/self/ns/pid")].join(":");
    else if (process.platform === "darwin") {
        const result = spawnSync("/usr/sbin/ioreg", ["-rd1", "-c", "IOPlatformExpertDevice"], { encoding: "utf8" });
        machine = result.stdout?.match(/"IOPlatformUUID"\s*=\s*"([^"]+)"/)?.[1] ?? "";
    } else throw new Error("Local stopped proof unavailable on this host");
    if (!machine) throw new Error("Trusted machine identity unavailable");
    return createHash("sha256").update(`${process.platform}:${hostname()}:${machine}`).digest("hex");
}
export class LocalInstanceProofProvider implements InstanceProofProvider {
    constructor(private readonly hostIdentity = localHostIdentity()) {}
    async proveStopped(instance: InstanceDescriptor): ReturnType<InstanceProofProvider["proveStopped"]> {
        const route = instance.proofRoute;
        if (route.kind !== "stdioSupervisor" || route.hostIdentity !== this.hostIdentity) return { status: "unknown" };
        for (const pid of [route.dispatcherPid, route.periPid]) {
            if (!Number.isSafeInteger(pid) || !pid || pid <= 1) return { status: "unknown" };
            try { process.kill(pid, 0); return { status: "unknown" }; }
            catch (error) { if ((error as NodeJS.ErrnoException).code !== "ESRCH") return { status: "unknown" }; }
        }
        return { status: "stopped", proof: { kind: "instanceStopped", instanceId: instance.instanceId,
            generationId: instance.generationId, evidenceId: `local-esrch:${this.hostIdentity}:${route.dispatcherPid}:${route.periPid}` } };
    }
}
