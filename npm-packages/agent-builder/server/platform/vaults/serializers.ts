import { millisToUnixSeconds, parseJsonColumn, parseMetadataColumn } from '../json';
import type { PlatformCredentialRow, PlatformVaultRow } from '../repositories/vaults';
import type { CredentialAuthDescriptor, CredentialNetworkingDescriptor } from './schemas';

export interface VaultResource {
  readonly id: string;
  readonly object: 'vault';
  /** Documented as nullable: an update with `name: null` clears the stored name. */
  readonly name: string | null;
  readonly metadata: Record<string, string>;
  readonly created_at: number;
}

export interface CredentialResource {
  readonly id: string;
  readonly object: 'vault.credential';
  readonly vault_id: string;
  readonly name: string;
  readonly auth: CredentialAuthDescriptor;
  readonly metadata: Record<string, string>;
  readonly created_at: number;
  readonly updated_at: number;
}

function storedDescriptor(raw: string): Record<string, unknown> {
  const stored = parseJsonColumn<unknown>(raw, null);
  if (typeof stored !== 'object' || stored === null || Array.isArray(stored)) return {};
  return stored as Record<string, unknown>;
}

function storedSecretName(descriptor: Record<string, unknown>): string {
  const name = descriptor.secret_name;
  return typeof name === 'string' ? name : '';
}

function storedNetworking(descriptor: Record<string, unknown>): CredentialNetworkingDescriptor {
  const networking = descriptor.networking;
  if (typeof networking !== 'object' || networking === null || Array.isArray(networking)) {
    return { type: 'unrestricted' };
  }
  const record = networking as Record<string, unknown>;
  if (record.type !== 'limited' || !Array.isArray(record.allowed_hosts)) {
    return { type: 'unrestricted' };
  }
  return {
    type: 'limited',
    allowed_hosts: record.allowed_hosts.filter((host): host is string => typeof host === 'string'),
  };
}

/**
 * Rebuilds the public auth description from the stored descriptor. Only non-secret fields are
 * emitted; any other stored field is dropped so the response can never grow into a secret channel.
 */
export function toCredentialAuth(row: PlatformCredentialRow): CredentialAuthDescriptor {
  const descriptor = storedDescriptor(row.auth);
  if (row.auth_kind === 'environment_variable') {
    return {
      type: 'environment_variable',
      secret_name: storedSecretName(descriptor),
      networking: storedNetworking(descriptor),
    };
  }
  const mcpServerUrl = descriptor.mcp_server_url;
  return {
    type: 'static_bearer',
    mcp_server_url: typeof mcpServerUrl === 'string' ? mcpServerUrl : '',
  };
}

export function toVaultResource(row: PlatformVaultRow): VaultResource {
  return {
    id: row.id,
    object: 'vault',
    name: row.name.length === 0 ? null : row.name,
    metadata: parseMetadataColumn(row.metadata),
    created_at: millisToUnixSeconds(row.created_at),
  };
}

/** Credential rows carry cipher/iv/tag columns that must never reach the wire. */
export function toCredentialResource(row: PlatformCredentialRow): CredentialResource {
  return {
    id: row.id,
    object: 'vault.credential',
    vault_id: row.vault_id,
    name: row.name,
    auth: toCredentialAuth(row),
    metadata: parseMetadataColumn(row.metadata),
    created_at: millisToUnixSeconds(row.created_at),
    updated_at: millisToUnixSeconds(row.updated_at),
  };
}
