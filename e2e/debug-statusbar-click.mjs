// 调试脚本 v2：测试输入区超长文本（wrap > 12 行）时点击状态栏模型段是否被 input_area 消费
import path from "node:path";
import { fileURLToPath } from "node:url";
import { TmuxTester } from "tui-tester";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const PROJECT_ROOT = path.resolve(__dirname, "..");
const DEV_SH = path.join(PROJECT_ROOT, "dev.sh");

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const tester = new TmuxTester({
  command: [DEV_SH],
  size: { cols: 120, rows: 40 },
  cwd: PROJECT_ROOT,
  env: {},
  debug: false,
  snapshotDir: path.join(PROJECT_ROOT, "e2e", "recordings"),
});

async function clickModelSegment(label) {
  const txt = await tester.getScreenText();
  const lines = txt.split("\n");
  const candidates = ["claude", "opus", "sonnet", "haiku", "gpt"];
  for (let y = lines.length - 1; y >= 0 && y >= lines.length - 10; y--) {
    for (const c of candidates) {
      const idx = lines[y].toLowerCase().indexOf(c.toLowerCase());
      if (idx !== -1) {
        console.log(`[${label}] 找到模型文本 "${c}" at (${idx}, ${y})`);
        await tester.click(idx + 2, y);
        console.log(`[${label}] 已点击 (${idx + 2}, ${y})`);
        return true;
      }
    }
  }
  console.log(`[${label}] 未找到模型文本！底部 6 行：`);
  for (let y = lines.length - 1; y >= 0 && y >= lines.length - 6; y--) {
    console.log(`  y=${y}: ${lines[y].slice(0, 100)}`);
  }
  return false;
}

async function popupVisible() {
  const txt = await tester.getScreenText();
  const lines = txt.split("\n");
  return lines.some((l) => /[❯●○]/.test(l) && /opus|sonnet|haiku|mini|fast/.test(l));
}

async function closePopup() {
  await tester.sendKey("escape");
  await sleep(800);
}

try {
  await tester.start();
  await sleep(5000);
  await tester.waitForText("AI operating system", { timeout: 30000, interval: 1000 }).catch(() => {});
  await sleep(2000);

  // ── 场景 1：输入框 13 行文本（pasteText 支持换行）后点击状态栏模型段 ──
  console.log("\n===== 场景 1：输入框 13 行文本 =====");
  await tester.sendKey("escape");
  await sleep(500);
  // 输入 13 行文本（每行 40 字符）
  const longText = Array.from({ length: 13 }, (_, i) => `line ${i}: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa`).join("\n");
  await tester.paste(longText);
  await sleep(1000);
  const clicked1 = await clickModelSegment("13行文本");
  if (clicked1) {
    await sleep(1500);
    const visible1 = await popupVisible();
    console.log(`[13行文本] 弹窗可见: ${visible1}`);
    await closePopup();
  }

  // ── 场景 2：输入框 5 行文本（正常多行）──
  console.log("\n===== 场景 2：输入框 5 行文本 =====");
  await tester.sendKey("escape");
  await sleep(500);
  await tester.sendKey("ctrl+u");
  await sleep(300);
  const shortText = Array.from({ length: 5 }, (_, i) => `line ${i}: bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb`).join("\n");
  await tester.paste(shortText);
  await sleep(1000);
  const clicked2 = await clickModelSegment("5行文本");
  if (clicked2) {
    await sleep(1500);
    const visible2 = await popupVisible();
    console.log(`[5行文本] 弹窗可见: ${visible2}`);
    await closePopup();
  }

  // ── 场景 3：输入框空 + 无消息（基线）──
  console.log("\n===== 场景 3：空输入（基线） =====");
  await tester.sendKey("escape");
  await sleep(500);
  const clicked3 = await clickModelSegment("空输入");
  if (clicked3) {
    await sleep(1500);
    const visible3 = await popupVisible();
    console.log(`[空输入] 弹窗可见: ${visible3}`);
    await closePopup();
  }
} finally {
  await tester.stop().catch(() => {});
  console.log("\n完成");
}
