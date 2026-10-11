import { expect, mock, test } from "bun:test";
import { mkdtempSync, readFileSync, readdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createHandler } from "./index";

for (const logLevel of ["none", "summary", "body"] as const) {
    test(`request file retains headers while ${logLevel} console logging keeps its budget`, async () => {
        const logDir = mkdtempSync(join(tmpdir(), "peri-gateway-diagnostics-"));
        const originalFetch = globalThis.fetch;
        const originalLog = console.log;
        const output: unknown[][] = [];
        const headers = {
            authorization: "Bearer synthetic-diagnostic-fixture",
            "x-api-key": "synthetic-api-key-fixture",
            cookie: "fixture=synthetic-cookie-value",
        };
        globalThis.fetch = mock(async () => new Response(JSON.stringify({ diagnostic: "x".repeat(4500) }), {
            headers: { "content-type": "application/json" },
        }));
        console.log = (...args: unknown[]) => { output.push(args); };
        try {
            const handler = createHandler({ port: 3456, openaiBase: "https://fixture.invalid", anthropicBase: "https://fixture.invalid", logLevel, logDir });
            const response = await handler(new Request("http://localhost/v1/chat/completions", {
                method: "POST", headers: { ...headers, "content-type": "application/json" }, body: JSON.stringify({ model: "fixture" }),
            }));
            await response.text();
            const directory = readdirSync(logDir)[0]!;
            const request = JSON.parse(readFileSync(join(logDir, directory, "request.json"), "utf8"));
            expect(request.headers).toMatchObject(headers);
            const loggedHeaders = output.find((args) => args.includes("  HEADERS:"));
            if (logLevel === "body") {
                expect(JSON.parse(String(loggedHeaders?.[2]))).toMatchObject(headers);
                expect(output.some((args) => args.some((value) => String(value).startsWith("  RESPONSE: (non-JSON, length=")))).toBe(true);
            } else {
                expect(loggedHeaders).toBeUndefined();
                if (logLevel === "none") expect(output).toHaveLength(0);
            }
        } finally {
            globalThis.fetch = originalFetch;
            console.log = originalLog;
            rmSync(logDir, { recursive: true, force: true });
        }
    });
}
