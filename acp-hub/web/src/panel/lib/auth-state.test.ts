import { afterEach, describe, expect, it } from 'vitest';
import {
  authInvalidation,
  clearAuthInvalidation,
  installPrincipalRole,
  principalRole,
  publishAuthInvalidation,
  readOnly,
} from './auth-state';

afterEach(() => {
  installPrincipalRole(null);
  clearAuthInvalidation();
});

describe('auth state', () => {
  it('is closed by default and grants mutation only to a full principal', () => {
    expect(principalRole()).toBeNull();
    expect(readOnly()).toBe(true);
    installPrincipalRole('read-only');
    expect(readOnly()).toBe(true);
    installPrincipalRole('full');
    expect(readOnly()).toBe(false);
  });

  it('publishes principal revocation and its reason as one atomic event', () => {
    installPrincipalRole('full');
    publishAuthInvalidation('token revoked');
    expect(principalRole()).toBeNull();
    expect(authInvalidation()).toMatchObject({ reason: 'token revoked' });
  });

  it('clears an invalidation without manufacturing a second event', () => {
    publishAuthInvalidation('server restarted');
    const event = authInvalidation();
    clearAuthInvalidation();
    expect(event).not.toBeNull();
    expect(authInvalidation()).toBeNull();
  });
});
