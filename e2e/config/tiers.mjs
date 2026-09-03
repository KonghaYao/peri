/**
 * E2E 分层门禁定义（L0 / L1 / L2）。
 *
 * - L0：PR 冒烟，串行、无 retry，首轮必须全绿
 * - L1：合并前，panels + tool-cards + smoke，低并发
 * - L2 / release：发版全量，允许有限首轮 flake（retry 后仍须全绿）
 */

/** @typedef {"l0"|"l1"|"l2"|"release"} TierId */

/** @type {Record<TierId, object>} */
export const TIERS = {
  l0: {
    id: "l0",
    label: "L0 PR 冒烟",
    description:
      "串行、无 retry；偏确定性用例（视口/工具卡/面板冻结），约 5～8 分钟。",
    files: [
      "tests/smoke/viewport-40x8.test.ts",
      "tests/panels/plugin-uninstall-no-freeze.test.ts",
      "tests/tool-cards/first-tool-stuck-running.test.ts",
      "tests/tool-cards/header-suffix-and-error.test.ts",
      "tests/tool-cards/edit-diff.test.ts",
    ],
    parallel: 1,
    retry: 0,
    maxFirstAttemptFailures: 0,
  },
  l1: {
    id: "l1",
    label: "L1 合并前",
    description: "smoke + panels + tool-cards；parallel 2，retry 1；首轮最多 1 个 flake。",
    dirs: ["smoke", "panels", "tool-cards"],
    parallel: 2,
    retry: 1,
    maxFirstAttemptFailures: 1,
  },
  l2: {
    id: "l2",
    label: "L2 发版全量",
    description: "全部用例；parallel 3，retry 1；首轮最多 2 个 flake，最终须 28/28。",
    all: true,
    parallel: 3,
    retry: 1,
    maxFirstAttemptFailures: 2,
  },
  release: {
    id: "release",
    label: "Release 门禁",
    description: "同 L2；用于 release-prep / CI 发版脚本别名。",
    all: true,
    parallel: 3,
    retry: 1,
    maxFirstAttemptFailures: 2,
  },
};

/**
 * @param {string} id
 * @param {string[]} allFiles 扫描得到的全部 tests 下 .test.ts 相对路径
 */
export function resolveTierFiles(id, allFiles) {
  const tier = TIERS[id];
  if (!tier) {
    throw new Error(`未知 tier: ${id}（可选: ${Object.keys(TIERS).join(", ")}）`);
  }
  if (tier.all) {
    return [...allFiles];
  }
  if (tier.files?.length) {
    return tier.files.slice();
  }
  if (tier.dirs?.length) {
    return allFiles.filter((f) =>
      tier.dirs.some((d) => f.startsWith(`tests/${d.replace(/^\/+|\/+$/g, "")}/`)),
    );
  }
  return [];
}

/**
 * @param {string} id
 */
export function getTier(id) {
  const tier = TIERS[id];
  if (!tier) {
    throw new Error(`未知 tier: ${id}`);
  }
  return tier;
}
