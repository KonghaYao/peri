import type { ResourceSnapshot } from '../contracts/client.js';
import type { ResourceId, ResourceScope } from '../identity.js';

/**
 * Read-only resource access for one authenticated scope.
 *
 * `null` means the resource does not exist in this scope; storage failures must reject instead of
 * being reported as a miss.
 */
export interface ResourceQueries {
  get(scope: ResourceScope, resourceId: ResourceId): Promise<ResourceSnapshot | null>;
}
