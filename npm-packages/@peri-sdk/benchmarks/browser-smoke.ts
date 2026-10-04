// Deterministic DOM smoke; no model, credentials or persistent Session Store.
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { server, closeFixture } from "./browser-fixture";

const profile = await mkdtemp(join(tmpdir(), "peri-yjs-chrome-"));
const chrome = Bun.spawn([
    process.env.CHROME_BIN ?? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "--headless=new", "--disable-gpu", "--remote-debugging-port=0", `--user-data-dir=${profile}`,
    "--no-first-run", "--no-default-browser-check", "about:blank",
], { stdout: "ignore", stderr: "ignore" });
let socket: WebSocket | undefined;
try {
    let port = "";
    for (let attempt = 0; attempt < 100; attempt++) {
        try {
            port = (await Bun.file(`${profile}/DevToolsActivePort`).text()).split("\n")[0]!;
            break;
        } catch { await Bun.sleep(50); }
    }
    if (!port) throw new Error("Chrome did not start");
    const target = await (await fetch(`http://127.0.0.1:${port}/json/new?about:blank`, { method: "PUT" })).json() as {
        webSocketDebuggerUrl: string;
    };
    socket = new WebSocket(target.webSocketDebuggerUrl);
    const pending = new Map<number, { resolve: (result: any) => void; reject: (error: unknown) => void }>();
    let serial = 0;
    const errors: unknown[] = [];
    socket.onmessage = ({ data }) => {
        const message = JSON.parse(String(data));
        if (message.id) {
            const waiter = pending.get(message.id);
            pending.delete(message.id);
            if (message.error) waiter?.reject(message.error);
            else waiter?.resolve(message.result);
        } else if (message.method === "Runtime.exceptionThrown") errors.push(message.params);
    };
    await new Promise((resolve, reject) => { socket!.onopen = resolve; socket!.onerror = reject; });
    const send = (method: string, params: object = {}): Promise<any> => new Promise((resolve, reject) => {
        const id = ++serial;
        const timer = setTimeout(() => { pending.delete(id); reject(new Error(`CDP timed out: ${method}`)); }, 10_000);
        pending.set(id, {
            resolve: (value) => { clearTimeout(timer); resolve(value); },
            reject: (error) => { clearTimeout(timer); reject(error); },
        });
        socket!.send(JSON.stringify({ id, method, params }));
    });
    const evaluate = async (expression: string) => {
        const result = await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true });
        if (result.exceptionDetails) throw result.exceptionDetails;
        return result.result.value;
    };
    await send("Runtime.enable");
    await send("Page.enable");
    await send("Page.navigate", { url: `http://127.0.0.1:${server.port}/?sessionId=smoke` });
    for (let attempt = 0; attempt < 150; attempt++) {
        if (await evaluate("document.querySelectorAll('#messages details').length > 100")) break;
        await Bun.sleep(100);
    }
    const initial = await evaluate(`({children:document.querySelector('#messages').children.length,
        tools:document.querySelectorAll('#messages details').length,error:document.querySelector('#error').textContent,
        status:document.querySelector('#status').textContent})`);
    const before = await evaluate("fetch('/stats').then(r=>r.json())");
    await evaluate("window.smokeTool=document.querySelectorAll('#messages details')[10]; window.smokeTool.open=true");
    for (let attempt = 0; attempt < 100; attempt++) {
        if ((await evaluate("fetch('/stats').then(r=>r.json())")).payloadReads === 1 &&
            await evaluate("window.smokeTool.querySelector('pre').textContent.length > 8000")) break;
        await Bun.sleep(20);
    }
    const expanded = await evaluate("({bodyBytes:window.smokeTool.querySelector('pre').textContent.length,open:window.smokeTool.open})");
    await evaluate("fetch('/advance',{method:'POST'}).then(r=>r.json())");
    await Bun.sleep(100);
    const updated = await evaluate(`({preserved:window.smokeTool.isConnected,open:window.smokeTool.open,
        tail:document.querySelector('#messages').textContent.includes('live-tail'),
        children:document.querySelector('#messages').children.length})`);
    await evaluate("document.querySelector('#messages > button').click()");
    await Bun.sleep(50);
    const after = await evaluate("({children:document.querySelector('#messages').children.length})");
    after.stats = await evaluate("fetch('/stats').then(r=>r.json())");
    await evaluate("fetch('/long',{method:'POST'}).then(r=>r.json())");
    for (let attempt = 0; attempt < 100; attempt++) {
        if (await evaluate("!!document.querySelector('button[data-download]')")) break;
        await Bun.sleep(20);
    }
    const longText = await evaluate(`(() => {
        const button = document.querySelector('button[data-download]');
        const originalUrl = URL.createObjectURL, originalClick = HTMLAnchorElement.prototype.click;
        let fullBytes = 0;
        URL.createObjectURL = blob => { fullBytes = blob.size; return originalUrl.call(URL, blob); };
        HTMLAnchorElement.prototype.click = () => {}; // Inspect the download without writing to the user's Downloads.
        try { button.click(); } finally { URL.createObjectURL = originalUrl; HTMLAnchorElement.prototype.click = originalClick; }
        return { visibleCharacters:button.parentElement.textContent.length, fullBytes };
    })()`);
    const result = { status: "passed", fixtureTools: 12000, initial, before, expanded, updated, after, longText, errors };
    if (initial.children !== 201 || initial.tools < 100 || initial.error || before.payloadReads !== 0 ||
        !expanded.open || expanded.bodyBytes < 8000 || !updated.preserved || !updated.open || !updated.tail ||
        updated.children !== 201 || after.children !== 401 || after.stats.payloadReads !== 1 ||
        longText.visibleCharacters > 66000 || longText.fullBytes !== 128 * 1024 || errors.length) {
        throw new Error(JSON.stringify(result));
    }
    console.log(JSON.stringify(result, null, 2));
} finally {
    socket?.close();
    chrome.kill("SIGTERM");
    await chrome.exited;
    await rm(profile, { recursive: true, force: true });
    await closeFixture();
}
