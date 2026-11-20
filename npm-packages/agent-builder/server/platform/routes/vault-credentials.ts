import { Hono } from 'hono';
import { platformError } from '../errors';
import { CredentialKeyError, type EncryptedSecret, encryptCredential } from '../hashing';
import { serializeJsonColumn, serializeMetadataColumn } from '../json';
import { paginate, parsePageRequest } from '../pagination';
import type { PlatformRepositories } from '../repositories';
import { parsePlatformJson } from '../request';
import { pathParam } from '../vaults/params';
import {
  CreateCredentialBody,
  CredentialAuthUnsupportedError,
  type ParsedCredentialAuth,
  parseCredentialAuth,
  parseCredentialSecretRotation,
  UpdateCredentialBody,
} from '../vaults/schemas';
import { credentialUnavailable } from '../vaults/secrets';
import { toCredentialResource } from '../vaults/serializers';

/** Routes for `/v1/vaults/{vault_id}/credentials`. */
export function createVaultCredentialRoutes(repos: PlatformRepositories): Hono {
  const routes = new Hono();

  routes.get('/', (c) => {
    const vaultId = pathParam(c, 'vault_id');
    if (repos.vaults.get(vaultId) === null) {
      return platformError(c, 'not_found', 'Vault not found', 404);
    }
    const request = parsePageRequest(c.req.query());
    const rows = repos.credentials.listByVault(vaultId, { ...request, limit: request.limit + 1 });
    const page = paginate(rows, { ...request, after: null }, (row) => row.id);
    return c.json({ ...page, data: page.data.map(toCredentialResource) });
  });

  routes.post('/', async (c) => {
    const vaultId = pathParam(c, 'vault_id');
    if (repos.vaults.get(vaultId) === null) {
      return platformError(c, 'not_found', 'Vault not found', 404);
    }
    const body = await parsePlatformJson(c, CreateCredentialBody);

    let auth: ParsedCredentialAuth;
    try {
      auth = parseCredentialAuth(body.auth);
    } catch (error) {
      if (error instanceof CredentialAuthUnsupportedError) {
        return platformError(
          c,
          'credential_auth_unsupported',
          'The credential auth type is not supported',
          400,
          'auth.type',
        );
      }
      throw error;
    }

    let secret: EncryptedSecret;
    try {
      secret = encryptCredential(auth.secret);
    } catch (error) {
      if (error instanceof CredentialKeyError) return credentialUnavailable(c);
      throw error;
    }

    const row = repos.credentials.create({
      vaultId,
      name: body.name,
      authKind: auth.descriptor.type,
      auth: serializeJsonColumn(auth.descriptor),
      secret,
      metadata: serializeMetadataColumn(body.metadata ?? {}),
    });
    return c.json(toCredentialResource(row), 201);
  });

  routes.get('/:credential_id', (c) => {
    const row = repos.credentials.getForVault(pathParam(c, 'vault_id'), pathParam(c, 'credential_id'));
    if (row === null) return platformError(c, 'not_found', 'Credential not found', 404);
    return c.json(toCredentialResource(row));
  });

  routes.post('/:credential_id', async (c) => {
    const vaultId = pathParam(c, 'vault_id');
    const credentialId = pathParam(c, 'credential_id');
    const existing = repos.credentials.getForVault(vaultId, credentialId);
    if (existing === null) return platformError(c, 'not_found', 'Credential not found', 404);

    const body = await parsePlatformJson(c, UpdateCredentialBody);

    let secret: EncryptedSecret | null = null;
    if (body.auth !== undefined) {
      let plaintext: string;
      try {
        plaintext = parseCredentialSecretRotation(body.auth, existing.auth_kind);
      } catch (error) {
        if (error instanceof CredentialAuthUnsupportedError) {
          return platformError(
            c,
            'credential_auth_unsupported',
            'The credential auth type is not supported',
            400,
            'auth.type',
          );
        }
        throw error;
      }
      try {
        secret = encryptCredential(plaintext);
      } catch (error) {
        if (error instanceof CredentialKeyError) return credentialUnavailable(c);
        throw error;
      }
    }

    // The descriptor, auth kind and name stay as stored: rotation replaces secret/metadata only.
    const row = repos.credentials.update(credentialId, {
      name: null,
      authKind: null,
      auth: null,
      secret,
      metadata: body.metadata === undefined ? null : serializeMetadataColumn(body.metadata),
    });
    if (row === null) return platformError(c, 'not_found', 'Credential not found', 404);
    return c.json(toCredentialResource(row));
  });

  routes.delete('/:credential_id', (c) => {
    const credentialId = pathParam(c, 'credential_id');
    if (!repos.credentials.deleteForVault(pathParam(c, 'vault_id'), credentialId)) {
      return platformError(c, 'not_found', 'Credential not found', 404);
    }
    return c.json({
      id: credentialId,
      object: 'vault.credential.deleted',
      deleted: true,
    });
  });

  return routes;
}
