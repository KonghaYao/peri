import {
  type Provider,
  type ProviderCreate,
  ProviderListResponse,
  ProviderResponse,
  type ProviderUpdate,
} from '@shared/provider';
import { type QueryClient, useQuery } from '@tanstack/react-query';
import { apiRequest, apiRequestVoid, jsonInit } from '@/lib/api-client';

export function useProviders() {
  return useQuery({
    queryKey: ['providers'],
    queryFn: async () => (await apiRequest('/api/providers', undefined, ProviderListResponse)).providers,
  });
}

// 凭据请求直接从短生命周期表单发送，不经过 TanStack mutation variables/cache。
export async function createProvider(input: ProviderCreate): Promise<Provider> {
  return (
    await apiRequest('/api/providers', { ...jsonInit('POST', input), cache: 'no-store' }, ProviderResponse)
  ).provider;
}
export async function updateProvider(id: string, input: ProviderUpdate, revision: number): Promise<Provider> {
  return (
    await apiRequest(
      `/api/providers/${id}`,
      {
        ...jsonInit('PUT', input),
        cache: 'no-store',
        headers: { 'content-type': 'application/json', 'If-Match': String(revision) },
      },
      ProviderResponse,
    )
  ).provider;
}
export async function getProvider(id: string): Promise<Provider> {
  return (await apiRequest(`/api/providers/${id}`, undefined, ProviderResponse)).provider;
}
export function deleteProvider(provider: Provider): Promise<void> {
  return apiRequestVoid(`/api/providers/${provider.id}`, {
    method: 'DELETE',
    headers: { 'If-Match': String(provider.revision) },
  });
}
export async function invalidateProviders(client: QueryClient): Promise<void> {
  await Promise.all([
    client.invalidateQueries({ queryKey: ['providers'] }),
    client.invalidateQueries({ queryKey: ['agent'] }),
    client.invalidateQueries({ queryKey: ['capabilities'] }),
  ]);
}
