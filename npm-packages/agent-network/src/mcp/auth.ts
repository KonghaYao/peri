import { BusinessError } from '../contracts/errors';
import { getActor, getCredentialByHash } from '../storage/queries';
import { hashToken } from '../storage/db';
import { PUBLIC_ACTOR_ID, type Principal } from '../domain/principal';

const BEARER = /^Bearer\s+(\S+)$/u;

/**
 * 从 Authorization 头解析当前凭据。
 *
 * 缺少 Authorization 时使用公共 actor，便于本地/测试客户端直接连接；
 * 显式携带但无效的 Bearer 仍返回未注册 principal，不降级为公共身份。
 */
export async function authenticate(db: D1Database, request: Request): Promise<Principal> {
  const header = request.headers.get('authorization');
  if (!header) {
    const actor = await getActor(db, PUBLIC_ACTOR_ID);
    return { credentialHash: '', actor, credential: null };
  }
  const match = BEARER.exec(header);
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
