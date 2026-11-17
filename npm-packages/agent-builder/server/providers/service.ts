import { type AgentProvider, type InlineAgentProvider, isReferenceProvider } from '@shared/agent';
import {
  type Provider,
  type ProviderCreate,
  ProviderCreate as ProviderCreateSchema,
  type ProviderUpdate,
  ProviderUpdate as ProviderUpdateSchema,
  providerOrigin,
  validProviderUrl,
} from '@shared/provider';
import { AppError } from '../errors';
import type { ProviderRecord, ProvidersRepository } from './repository';

export interface ResolvedProvider {
  readonly provider: InlineAgentProvider;
  readonly apiKey: string | undefined;
  readonly providerId: string | null;
}

export class ProviderService {
  readonly #runtime = new Map<string, string>();
  readonly repository: ProvidersRepository;
  constructor(repository: ProvidersRepository) {
    this.repository = repository;
  }

  #credential(record: ProviderRecord): string | undefined {
    // v2 的旧 env 行只读兼容；显式保存后才转换为 SQLite 凭据。
    return record.credentialSource === 'local' ? (record.apiKey ?? undefined) : undefined;
  }

  #dto(record: ProviderRecord): Provider {
    return {
      id: record.id,
      name: record.name,
      type: record.type,
      baseUrl: record.baseUrl,
      credentialSource: 'local',
      revision: record.revision,
      hasCredential: this.#credential(record) !== undefined,
      agentCount: this.repository.agentCount(record.id),
      runtimeCount: [...this.#runtime.values()].filter((id) => id === record.id).length,
    };
  }

  list(): Provider[] {
    return this.repository.list().map((record) => this.#dto(record));
  }
  get(id: string): Provider {
    const record = this.repository.get(id);
    if (!record) throw new AppError('not_found', 'Provider not found');
    return this.#dto(record);
  }

  create(input: ProviderCreate): Provider {
    input = ProviderCreateSchema.parse(input);
    return this.repository.transaction(() => {
      const record: ProviderRecord = {
        id: crypto.randomUUID(),
        name: input.name,
        type: input.type,
        baseUrl: input.baseUrl,
        credentialSource: input.credentialSource,
        apiKey: this.#nextKey(input, null),
        revision: 1,
      };
      this.repository.create(record);
      return this.#dto(record);
    });
  }

  #nextKey(input: ProviderCreate | ProviderUpdate, previous: ProviderRecord | null): string | null {
    if (input.credential?.action === 'replace') return input.credential.value;
    if (input.credential?.action === 'clear') return null;
    return previous?.credentialSource === 'local' ? previous.apiKey : null;
  }

  update(id: string, input: ProviderUpdate, revision: number): Provider {
    input = ProviderUpdateSchema.parse(input);
    return this.repository.transaction(() => {
      const previous = this.repository.get(id);
      if (!previous) throw new AppError('not_found', 'Provider not found');
      if (previous.revision !== revision) throw new AppError('revision_conflict', 'Stale Provider revision');
      const record: ProviderRecord = {
        ...previous,
        name: input.name,
        baseUrl: input.baseUrl,
        credentialSource: input.credentialSource,
        apiKey: this.#nextKey(input, previous),
        revision: revision + 1,
      };
      // 确认不代替 URL 校验；本地单用户明确允许改变现有凭据的发送范围。
      const reusesCredential = this.#credential(record) === this.#credential(previous);
      if (
        reusesCredential &&
        this.#credential(record) !== undefined &&
        providerOrigin(previous.type, previous.baseUrl) !== providerOrigin(record.type, record.baseUrl) &&
        input.confirmCredentialOriginChange !== true
      ) {
        throw new AppError(
          'credential_origin_confirmation_required',
          'Confirm credential destination change',
        );
      }
      if (!this.repository.update(record, revision))
        throw new AppError('revision_conflict', 'Stale Provider revision');
      return this.#dto(record);
    });
  }

  delete(id: string, revision: number): void {
    this.repository.transaction(() => {
      const current = this.repository.get(id);
      if (!current) throw new AppError('not_found', 'Provider not found');
      if (current.revision !== revision) throw new AppError('revision_conflict', 'Stale Provider revision');
      if (this.repository.agentCount(id) > 0 || [...this.#runtime.values()].includes(id)) {
        throw new AppError('provider_in_use', 'Provider is still referenced');
      }
      if (!this.repository.delete(id, revision))
        throw new AppError('revision_conflict', 'Stale Provider revision');
    });
  }

  resolve(provider: AgentProvider): ResolvedProvider {
    if (!isReferenceProvider(provider))
      return {
        provider: structuredClone(provider),
        apiKey: undefined,
        providerId: null,
      };
    const record = this.repository.get(provider.providerId);
    if (!record) throw new AppError('not_found', 'Referenced Provider not found');
    if (record.baseUrl !== null && !validProviderUrl(record.baseUrl))
      throw new AppError('invalid_input', 'Stored Provider URL is invalid');
    return {
      providerId: record.id,
      apiKey: this.#credential(record),
      provider: {
        id: record.id,
        type: record.type,
        model: provider.model,
        ...(record.baseUrl === null ? {} : { baseUrl: record.baseUrl }),
      },
    };
  }

  retain(chatId: string, providerId: string): void {
    this.#runtime.set(chatId, providerId);
  }
  release(chatId: string): void {
    this.#runtime.delete(chatId);
  }
}
