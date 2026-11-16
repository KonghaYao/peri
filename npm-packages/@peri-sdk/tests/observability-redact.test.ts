import { describe, expect, test } from "bun:test";
import type { SandboxObservation } from "../src/contracts/observability";
import { observeBackend } from "../src/entries/observe";
import { SandboxManager } from "../src/sandbox/manager";
import type {
    SandboxAdapter,
    SandboxAdapterInstance,
    SandboxAdapterStartOptions,
} from "../src/sandbox/types";
import { redactObservationText } from "../src/util/observability";

const BASIC_VALUE = "dXNlcjpwYXNz";
const API_KEY = "key-leak";
const FRAGMENT_TOKEN = "hash-secret";
const QUERY_TOKEN = "query-secret";
const USERINFO_PASSWORD = "userinfo-password-leak";
const BEARER_TOKEN = "bearer-token-must-not-appear";
const SANDBOX_ID = `sbx_${"c".repeat(32)}`;

const REPORTED_LEAKS = [
    BASIC_VALUE,
    API_KEY,
    FRAGMENT_TOKEN,
    QUERY_TOKEN,
    USERINFO_PASSWORD,
    BEARER_TOKEN,
];

/** 覆盖验证报告里的四类泄露：Basic 值、敏感 header、URL fragment、自由文本。 */
const LEAKY_TEXT =
    `sandbox failed: Authorization: Basic ${BASIC_VALUE}; ` +
    `X-Api-Key: ${API_KEY}; ` +
    `https://example.com/mcp#access_token=${FRAGMENT_TOKEN}; ` +
    `Bearer ${BEARER_TOKEN}`;

describe("redactObservationText", () => {
    test("redacts the reported leaks completely and is idempotent", () => {
        const cases = [
            {
                input: `Authorization: Basic ${BASIC_VALUE}`,
                expected: "Authorization: [redacted]",
            },
            {
                input: `X-Api-Key: ${API_KEY}`,
                expected: "X-Api-Key: [redacted]",
            },
            {
                input: `https://example.com/mcp#access_token=${FRAGMENT_TOKEN}`,
                expected: "https://example.com/mcp",
            },
        ];

        for (const { input, expected } of cases) {
            const output = redactObservationText(input);
            expect(output).toBe(expected);
            expect(redactObservationText(output)).toBe(output);
        }
    });

    test("redacts full values of common sensitive headers", () => {
        const headers = [
            ["Proxy-Authorization", `Digest username="alice", nonce="n0nce"`],
            ["X-Auth-Token", "xauth-secret"],
            ["Cookie", "session=cookie-secret; theme=dark"],
            ["Set-Cookie", "sid=setcookie-secret; HttpOnly"],
            ["Credential", "credential-secret"],
            ["Password", "password-secret"],
            ["Secret", "secret-value"],
            ["Api-Key", "api-key-secret"],
        ];

        for (const [name, value] of headers) {
            const output = redactObservationText(`${name}: ${value}`);
            expect(output).toBe(`${name}: [redacted]`);
            expect(output).not.toContain(value!);
            expect(redactObservationText(output)).toBe(output);
        }
    });

    test("redacts standalone Bearer and Basic fragments", () => {
        const bearer = redactObservationText("upstream said Bearer abc.def.ghi");
        expect(bearer).toBe("upstream said Bearer [redacted]");
        expect(redactObservationText(bearer)).toBe(bearer);

        const basic = redactObservationText(`payload Basic ${BASIC_VALUE}`);
        expect(basic).toBe("payload Basic [redacted]");
        expect(redactObservationText(basic)).toBe(basic);
    });

    test("redacts non-URL assignments including quoted and underscored keys", () => {
        const secrets = ["abc", "def", "ghi", "jkl", "mno", "pqr", "json-secret"];
        const output = redactObservationText(
            "start failed: access_token=abc refresh_token=def api_key=ghi " +
                "secret=jkl password=mno token=pqr " +
                '{"token":"json-secret"} client_secret=underscored-secret',
        );
        for (const secret of [...secrets, "underscored-secret"])
            expect(output).not.toContain(secret);
        expect(redactObservationText(output)).toBe(output);
    });

    test("collapses URLs in text to origin + pathname", () => {
        const input =
            `connect https://alice:${USERINFO_PASSWORD}@example.com/mcp` +
            `?token=${QUERY_TOKEN}&x=1#${FRAGMENT_TOKEN} failed`;
        const output = redactObservationText(input);
        expect(output).toContain("https://example.com/mcp");
        for (const secret of [USERINFO_PASSWORD, QUERY_TOKEN, FRAGMENT_TOKEN])
            expect(output).not.toContain(secret);
        expect(redactObservationText(output)).toBe(output);

        const redis = redactObservationText(
            `dial redis://alice:${USERINFO_PASSWORD}@cache:6379/0` +
                `?token=${QUERY_TOKEN} failed`,
        );
        expect(redis).toContain("redis://cache:6379/0");
        for (const secret of [USERINFO_PASSWORD, QUERY_TOKEN])
            expect(redis).not.toContain(secret);
        expect(redactObservationText(redis)).toBe(redis);
    });

    test("leaves unrelated text unchanged", () => {
        const input = "sandbox failed: timeout after 5000ms (stop_incomplete)";
        expect(redactObservationText(input)).toBe(input);
    });
});

class FailingCloseInstance implements SandboxAdapterInstance {
    readonly workspace: {
        id: string;
        hostPath: string;
        runtimePath: string;
    };
    readonly mcpServer: {
        type: "http";
        url: string;
        headers: Record<string, string>;
    };

    constructor(readonly sandboxId: string) {
        this.workspace = {
            id: sandboxId,
            hostPath: `/tmp/sandboxes/${sandboxId}`,
            runtimePath: `/tmp/sandboxes/${sandboxId}`,
        };
        this.mcpServer = {
            type: "http",
            url: `http://127.0.0.1:4401/mcp?token=${QUERY_TOKEN}`,
            headers: { Authorization: `Bearer ${BEARER_TOKEN}` },
        };
    }

    async close(): Promise<void> {
        throw new Error(LEAKY_TEXT);
    }
}

class FailingCloseAdapter implements SandboxAdapter {
    async start(
        options: SandboxAdapterStartOptions,
    ): Promise<SandboxAdapterInstance> {
        return new FailingCloseInstance(options.sandboxId);
    }
}

describe("sandbox snapshot output boundary", () => {
    test("SandboxManager.snapshot redacts close() errors", async () => {
        const manager = new SandboxManager({ adapter: new FailingCloseAdapter() });
        const handle = await manager.start({ sandboxId: SANDBOX_ID });
        await expect(handle.stop()).rejects.toThrow();

        const snapshot = manager.snapshot();
        expect(snapshot).toHaveLength(1);
        expect(snapshot[0]!.state).toBe("failed");
        expect(snapshot[0]!.mcpEndpoint).toBeNull();
        expect(snapshot[0]!.lastError?.message).toContain("[redacted]");

        const serialized = JSON.stringify(snapshot);
        for (const secret of REPORTED_LEAKS)
            expect(serialized).not.toContain(secret);
    });

    test("observeBackend redacts custom SandboxObservationSource output", () => {
        const observations: SandboxObservation[] = [
            {
                sandboxId: SANDBOX_ID,
                state: "failed",
                serverName: "workspace",
                mcpEndpoint:
                    `https://alice:${USERINFO_PASSWORD}@example.com/mcp` +
                    `?token=${QUERY_TOKEN}#${FRAGMENT_TOKEN}`,
                workspace: { id: "ws", runtimePath: "/workspace" },
                lastError: {
                    code: "start_failed",
                    message: LEAKY_TEXT,
                },
            },
            {
                // 未 ready 的 sandbox 没有 endpoint，也会被列为 orphan。
                sandboxId: `sbx_${"d".repeat(32)}`,
                state: "starting",
                serverName: "workspace",
                mcpEndpoint: null,
                workspace: null,
                lastError: null,
            },
        ];
        const snapshot = observeBackend({
            chats: { observe: () => [] },
            sandboxes: { snapshot: () => observations },
        });

        expect(snapshot.orphanSandboxes.map((item) => item.sandboxId)).toEqual([
            SANDBOX_ID,
            `sbx_${"d".repeat(32)}`,
        ]);
        expect(snapshot.orphanSandboxes[0]!.mcpEndpoint).toBe(
            "https://example.com/mcp",
        );
        expect(snapshot.orphanSandboxes[0]!.lastError?.message).toContain(
            "[redacted]",
        );

        const serialized = JSON.stringify(snapshot);
        for (const secret of REPORTED_LEAKS)
            expect(serialized).not.toContain(secret);
    });
});
