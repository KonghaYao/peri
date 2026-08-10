// timeline_study.ts — 关键现象时间线标记
// 口径: 会话级时间(threads.created_at/updated_at, 绝对); 消息引文(会话内可见序号, 相对)
// 输出: reports/timeline.md 追加用 JSON + 终端摘要
import { Database } from "bun:sqlite";
import { homedir } from "os";
import { join } from "path";
import { writeFileSync } from "fs";

const db = new Database(join(homedir(), ".peri/threads/threads.db"), { readonly: true });

// 1. 机制时间轴: 超长会话 compact 痕迹首现
const compactOnset = db.query(
  `SELECT t.id, t.created_at, t.message_count,
     (SELECT COUNT(*) FROM messages m WHERE m.thread_id=t.id AND m.excluded=1) excl,
     (SELECT COUNT(*) FROM messages m WHERE m.thread_id=t.id AND m.truncated=1) trunc,
     (SELECT COUNT(*) FROM messages m WHERE m.thread_id=t.id AND m.projection IS NOT NULL AND m.projection!='') proj
   FROM threads t WHERE t.parent_thread_id IS NULL AND t.hidden=0 AND t.message_count>=500
   ORDER BY t.created_at`
).all() as any[];
const firstCompact = compactOnset.find((t) => (t.excl > 0 || t.trunc > 0 || t.proj > 0));
const lastBare = [...compactOnset].reverse().find((t) => t.excl === 0 && t.trunc === 0 && t.proj === 0);

// 2. 抽样 13 会话的区间与关键引文位置
const SAMPLE = [
  { id: "019ea51f-bd07-71c3-9cef-45c35721c758", ev: [["用户发现白名单过滤转向", 258], ["用户贴日志定位闭包bug", 406], ["验收 ok,修复了", 444], ["用户否定删除方案", 494], ["per-agent关联未闭环", 508]] },
  { id: "019f257b-d074-7910-ad27-729cfa44aed1", ev: [["标记设计图为v2", 59], ["删快捷键表确立原则", 283], ["emoji收敛第二轮", 376], ["emoji收敛(用户纠正)", 410], ["外部review引入", 580], ["确认可大重构", 660], ["/handoff", 662]] },
  { id: "019f26d4-bf0a-74f2-a56c-aaaa94452d03", ev: [["放权声明(全部重写)", 1], ["用户信任峰值:直接全部重写", 120], ["code-reviewer 8项问题", 346], ["LLM API故障", 905], ["/handoff 无验收", 942]] },
  { id: "019f1c4e-418b-7f82-9e69-043462e794fb", ev: [["幻觉误判被纠正", 421], ["用户确认LLM幻觉", 433], ["用户给防幻觉方案", 435], ["输入框批评", 677], ["授权加速", 691], ["5-phase完成", 726], ["双光标回归", 727]] },
  { id: "019f259c-6be4-7d33-bb8d-caf7a4a766fd", ev: [["blockquote反色方案采纳", 402], ["审美批评:不够github", 484], ["信任杀手:example没改到", 918], ["没有生效", 950], ["表格复用失败回退", 1134]] },
  { id: "019f2a3d-5e02-7b93-9154-abd285ed2026", ev: [["闪退首报", 74], ["修复buffer溢出", 134], ["发火:为何不遵守", 523], ["good,完美", 569], ["ultra-batch分析", 783], ["压缩commit收尾", 849]] },
  { id: "019f9497-92bc-7fd1-a585-837cf198692e", ev: [["并行派发3后台explorer", 18], ["高置信问题播报", 254], ["输出md文件", 584], ["报告交付", 660]] },
  { id: "019fad4c-c396-7161-9fba-7136957c58ed", ev: [["compact摘要恢复", 194], ["首轮review P1泄露", 500], ["二轮review 乱序阻断", 661], ["三轮无阻断", 760], ["提交代码", 793]] },
  { id: "019fb6de-99f1-72d2-babe-bc42b50f23fc", ev: [["compact恢复22h上下文", 1483], ["并行coder文件不相交", 1505], ["用户问未完成事项", 1544], ["checkbox滞后回填", 1564], ["授权commit+task8b", 1565]] },
  { id: "019fbd31-2d34-7433-8143-a7e51ed140c0", ev: [["dump屏幕发现zh-CN", 620], ["手动tmux复现", 715], ["event_sink漏映射定位", 715], ["e2e全绿", 805], ["提交", 818]] },
  { id: "019fccee-5bc3-7ab2-b490-4b081d4bfb4d", ev: [["架构方案否决重定向", 762], ["自死锁发现", 762], ["Shell来源隔离", 800], ["回归测试", 845], ["提交修复", 880]] },
  { id: "019fdb17-9938-7730-8a0e-53d1a291ef96", ev: [["用户抱怨test慢", 1544], ["卡死了", 1544], ["用户kill workflow接管", 1544], ["全部套件全绿351测试", 1562], ["整理提交", 1575], ["/handoff", 1601]] },
  { id: "019fe475-5bd7-7a13-be4a-664647cc273b", ev: [["git mv machine instance", 2507], ["e2e首跑失败", 2616], ["系统性遗漏确认:wire", 2632], ["用户:继续", 2651], ["outbox修复中(当前)", 2680]] },
];

interface Row { thread_id: string; rowid: number; excluded: number }
const out: any = { meta: { generated_at: new Date().toISOString() }, compact_onset: firstCompact?.created_at, last_bare: lastBare?.created_at, events: [] as any[] };

for (const s of SAMPLE) {
  const t = db.query(`SELECT * FROM threads WHERE id=?`).get(s.id) as any;
  if (!t) { console.error("缺:", s.id); continue; }
  // compact 痕迹(来自 messages 表)
  const cm = db.query(
    `SELECT SUM(excluded) excl, SUM(truncated) trunc, SUM(CASE WHEN projection IS NOT NULL AND projection!='' THEN 1 ELSE 0 END) proj
     FROM messages WHERE thread_id=?`
  ).get(s.id) as any;
  const compactTag = [
    (cm?.excl ?? 0) > 0 ? `excluded=${cm.excl}` : "",
    (cm?.proj ?? 0) > 0 ? `projection=${cm.proj}` : "",
    (cm?.trunc ?? 0) > 0 ? `truncated=${cm.trunc}` : "",
  ].filter(Boolean).join(" ") || "无";
  // 可见消息计数 → 引文相对位置
  const rows = db.query(`SELECT rowid, excluded FROM messages WHERE thread_id=? ORDER BY rowid`).all(s.id) as Row[];
  const vis: { rowid: number; idx: number }[] = [];
  let idx = 0;
  for (const r of rows) { if (!r.excluded) { idx++; vis.push({ rowid: r.rowid, idx }); } }
  const byRowid = new Map(vis.map((v) => [v.rowid, v.idx]));
  const events = s.ev.map(([label, seq]: any) => {
    // seq 是可见消息序号 → 直接可用
    const pos = seq / Math.max(1, t.message_count);
    const phase = pos < 0.33 ? "前" : pos < 0.67 ? "中" : "后";
    return { label, seq, total: t.message_count, pos: Number(pos.toFixed(2)), phase };
  });
  out.events.push({
    id: s.id.slice(0, 8),
    title: t.title,
    created: t.created_at,
    updated: t.updated_at,
    msgs: t.message_count,
    compact: compactTag,
    events,
  });
}

writeFileSync(join(import.meta.dir, "data", "timeline.json"), JSON.stringify(out, null, 2));
console.log(`compact 痕迹首现(超长会话): ${firstCompact?.created_at} (${firstCompact?.id.slice(0, 8)})`);
console.log(`最后一个无compact超长会话: ${lastBare?.created_at} (${lastBare?.id.slice(0, 8)})`);
for (const e of out.events) {
  console.log(`\n${e.created.slice(0, 16)} → ${e.updated.slice(0, 16)} | ${e.id} | ${e.msgs}msgs | ${e.compact}`);
  for (const ev of e.events) console.log(`   [${ev.seq}/${ev.total} ${ev.phase}] ${ev.label}`);
}
db.close();
