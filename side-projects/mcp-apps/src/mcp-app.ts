// UI side: runs inside the host's sandboxed iframe and talks to the host
// over the same JSON-RPC base protocol used by MCP.
//
// The clock keeps ticking between syncs by extrapolating from the last
// server-provided timestamp (serverMs + elapsed wall time), and every
// "SYNC" press / AUTO tick is a real ui-initiated tools/call round-trip.
import { App } from "@modelcontextprotocol/ext-apps";

const clockEl = document.getElementById("clock")!;
const dateEl = document.getElementById("date")!;
const localEl = document.getElementById("local")!;
const statusDot = document.getElementById("status-dot")!;
const statusText = document.getElementById("status-text")!;
const lastSyncEl = document.getElementById("last-sync")!;
const syncBtn = document.getElementById("sync-btn") as HTMLButtonElement;
const autoToggle = document.getElementById("auto-toggle") as HTMLInputElement;

const app = new App({
  name: "Neon Server Clock",
  version: "1.0.0",
  description: "MCP Apps demo — server time display with neon dashboard UI",
});

const pad = (n: number) => String(n).padStart(2, "0");

let serverBase: { serverMs: number; wallMs: number } | null = null;
let autoTimer: number | null = null;
let syncing = false;

function setStatus(state: string, text: string) {
  statusDot.dataset.state = state;
  statusText.textContent = text;
}

function nowServerMs(): number | null {
  if (!serverBase) return null;
  return serverBase.serverMs + (performance.now() - serverBase.wallMs);
}

function onServerTime(iso: string) {
  const t = Date.parse(iso);
  if (Number.isNaN(t)) {
    setStatus("error", "BAD SERVER TIME");
    return;
  }
  serverBase = { serverMs: t, wallMs: performance.now() };
  const d = new Date(t);
  lastSyncEl.textContent = `LAST SYNC ${d.toUTCString().slice(17, 25)} UTC`;
}

function render() {
  const t = nowServerMs();
  if (t == null) {
    clockEl.textContent = "--:--:--";
    return;
  }
  const d = new Date(t);
  const hhmmss = `${pad(d.getUTCHours())}:${pad(d.getUTCMinutes())}:${pad(d.getUTCSeconds())}`;
  if (clockEl.textContent !== hhmmss) {
    clockEl.textContent = hhmmss;
    clockEl.classList.remove("tick");
    void clockEl.offsetWidth; // restart the pulse animation
    clockEl.classList.add("tick");
  }
  const utc = d.toUTCString();
  dateEl.textContent = `${utc.slice(0, 3).toUpperCase()} ${utc.slice(5, 16)}`;
  const l = new Date();
  localEl.textContent = `LOCAL ${pad(l.getHours())}:${pad(l.getMinutes())}:${pad(l.getSeconds())} · ${l.toString().slice(25, 31)}`;
}

async function sync() {
  if (syncing) return;
  syncing = true;
  syncBtn.disabled = true;
  setStatus("syncing", "CALLING get-time…");
  try {
    const result = await app.callServerTool({ name: "get-time", arguments: {} });
    const text = result.content?.find((c) => c.type === "text")?.text;
    if (text) {
      onServerTime(text);
      setStatus("connected", "LINK ESTABLISHED");
    } else {
      setStatus("error", "NO TEXT RESULT");
    }
  } catch (e) {
    setStatus("error", `ERR ${String((e as Error)?.message ?? e).slice(0, 400)}`);
  } finally {
    syncing = false;
    syncBtn.disabled = false;
  }
}

// Host-initiated tool results land here (e.g. when the agent calls get-time).
app.ontoolresult = (result) => {
  const text = result.content?.find((c) => c.type === "text")?.text;
  if (text) onServerTime(text);
};

syncBtn.addEventListener("click", sync);
autoToggle.addEventListener("change", () => {
  if (autoToggle.checked) {
    sync();
    autoTimer = window.setInterval(sync, 3000);
    setStatus("connected", "AUTO SYNC 3s");
  } else if (autoTimer != null) {
    clearInterval(autoTimer);
    autoTimer = null;
    setStatus("connected", "LINK ESTABLISHED");
  }
});

setInterval(render, 200);
setStatus("connecting", "CONNECTING…");
app
  .connect()
  .then(() => {
    setStatus("connected", "LINK ESTABLISHED");
    sync(); // initial sync round-trip
  })
  .catch((e) => setStatus("error", `ERR ${String((e as Error)?.message ?? e).slice(0, 400)}`));
