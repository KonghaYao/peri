import { BusinessError } from '../contracts/errors';
import { getActor, getCredentialByHash } from '../storage/queries';
import { hashToken } from '../storage/db';
import type { Principal } from '../domain/principal';

const BEARER = /^Bearer\s+(\S+)$/u;

/**
 * 从 Authorization 头解析当前凭据。
 *
 * 缺少凭据时返回未注册 principal（仅 bootstrap 工具可用）；凭据存在但已撤销时立即失败。
 */
export async function authenticate(db: D1Database, request: Request): Promise<Principal> {
  const header = request.headers.get('authorization');
  const match = header ? BEARER.exec(header) : null;
  if (!match?.[1]) {
    return { credentialHash: '', actor: null, credential: null };
  }
  const credentialHash = await hashToken(match[1]);
  const credential = await getCredentialByHash(db, credentialHash);
  if (!credential) {
    return { credentialHash, actor: null, credential: null };
  }
  if (credential.revoked_at !== null) {
    throw new BusinessError('CREDENTIAL_INVALID', '凭据已撤销');
  }
  const actor = await getActor(db, credential.actor_id);
  if (!actor) throw new BusinessError('CREDENTIAL_INVALID', '凭据绑定的 actor 不存在');
  return { credentialHash, actor, credential };
}

/** 写工具与私有资源要求已注册身份。 */
export function requireActor(principal: Principal): NonNullable<Principal['actor']> {
  if (!principal.actor) {
    throw new BusinessError('CREDENTIAL_INVALID', '当前凭据尚未注册；先调用 network_register');
  }
  return principal.actor;
}
