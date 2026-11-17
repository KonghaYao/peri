import { z } from 'zod';

export const ProviderType = z.enum(['anthropic', 'openai']);
export type ProviderType = z.infer<typeof ProviderType>;
export const CredentialSource = z.literal('local').default('local');

export function validProviderUrl(raw: string): boolean {
  if ([...raw].some((char) => char.charCodeAt(0) < 32 || char.charCodeAt(0) === 127)) return false;
  if (raw !== raw.trim() || /[\s\\?#]/.test(raw) || !/^https?:\/\//.test(raw)) return false;
  try {
    const url = new URL(raw);
    if (url.username || url.password || url.search || url.hash || raw.split('/')[2]?.includes('@'))
      return false;
    const loopback =
      url.hostname === 'localhost' || url.hostname === '[::1]' || /^127(?:\.\d{1,3}){3}$/.test(url.hostname);
    return url.protocol === 'https:' || (url.protocol === 'http:' && loopback);
  } catch {
    return false;
  }
}

export const ProviderBaseUrl = z.string().refine(validProviderUrl, {
  message: '请输入无凭据、query、fragment 的 HTTPS URL；HTTP 仅允许 loopback，不接受首尾空格',
});

export const ProviderMetadata = z.strictObject({
  name: z.string().trim().min(1).max(80),
  type: ProviderType,
  baseUrl: ProviderBaseUrl.nullable().default(null),
  credentialSource: CredentialSource,
});
export const CredentialOperation = z.discriminatedUnion('action', [
  z.strictObject({
    action: z.literal('replace'),
    value: z.string().refine((value) => value.trim().length > 0 && value.trim() !== '[redacted]'),
  }),
  z.strictObject({ action: z.literal('clear') }),
]);
export const ProviderCreate = ProviderMetadata.extend({ credential: CredentialOperation.optional() });
export type ProviderCreate = z.output<typeof ProviderCreate>;
export const ProviderUpdate = ProviderMetadata.omit({ type: true }).extend({
  credential: CredentialOperation.optional(),
  confirmCredentialOriginChange: z.boolean().optional(),
});
export type ProviderUpdate = z.output<typeof ProviderUpdate>;

export const Provider = ProviderMetadata.extend({
  id: z.uuid(),
  revision: z.number().int().positive(),
  hasCredential: z.boolean(),
  agentCount: z.number().int().nonnegative(),
  runtimeCount: z.number().int().nonnegative(),
});
export type Provider = z.infer<typeof Provider>;
export const ProviderResponse = z.strictObject({ provider: Provider });
export const ProviderListResponse = z.strictObject({ providers: z.array(Provider) });

export const MODEL_SUGGESTIONS: Record<ProviderType, string> = {
  anthropic: 'claude-sonnet-4-5',
  openai: 'gpt-5',
};

// 默认 origin 仅用于凭据发送范围确认；实际 SDK endpoint 仍由协议 adapter 决定。
export function providerOrigin(type: ProviderType, baseUrl: string | null): string {
  return new URL(baseUrl ?? (type === 'anthropic' ? 'https://api.anthropic.com' : 'https://api.openai.com'))
    .origin;
}
