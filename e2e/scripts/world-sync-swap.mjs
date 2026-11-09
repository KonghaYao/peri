import { randomUUID } from 'node:crypto';
import { rename, rm } from 'node:fs/promises';

/**
 * 原子替换 world 目录：先把旧目录移走，再装入 staging，校验通过后才删除旧目录。
 * 任一步失败都回滚；若回滚本身失败，错误里给出旧 world 的保留位置，绝不静默删除。
 */
export async function replaceWorldDirectory(worldDir, staged, { renameImpl = rename, rmImpl = rm, validate } = {}) {
  const previous = `${worldDir}.previous-${randomUUID()}`;
  await renameImpl(worldDir, previous);
  try {
    await renameImpl(staged, worldDir);
    if (validate) await validate(worldDir);
  } catch (error) {
    const failed = `${worldDir}.failed-${randomUUID()}`;
    let movedAway = true;
    try { await renameImpl(worldDir, failed); } catch { movedAway = false; }
    try {
      await renameImpl(previous, worldDir);
    } catch (rollbackError) {
      throw new AggregateError(
        [error, rollbackError],
        `world 替换失败且回滚失败：旧 world 保留在 ${previous}；失败的新 world 位于 ${movedAway ? failed : worldDir}`,
      );
    }
    if (movedAway) await rmImpl(failed, { recursive: true, force: true }).catch(() => {});
    throw error;
  }
  let removed = true;
  try { await rmImpl(previous, { recursive: true, force: true }); }
  catch { removed = false; }
  return { previous, removed };
}
