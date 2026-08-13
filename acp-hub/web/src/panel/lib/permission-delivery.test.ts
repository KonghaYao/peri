import { afterEach, describe, expect, it } from 'vitest';
import {
  failPermissionDecision,
  markPermissionDecisionUncertain,
  permissionDecisions,
  resetPermissionDecisions,
  retryPermissionDecision,
  retainProjectedPermissions,
  startPermissionDecision,
} from './permission-delivery';

afterEach(resetPermissionDecisions);

describe('permission delivery', () => {
  it('accepts only the first decision for one permission identity', () => {
    expect(startPermissionDecision('allow-1', 'permission-1', 'allow')).toBe(true);
    expect(startPermissionDecision('deny-1', 'permission-1', 'deny')).toBe(false);
    expect(permissionDecisions().get('permission-1')).toMatchObject({ commandId: 'allow-1', decision: 'allow', phase: 'pending', retryable: false });
  });

  it('keeps unrelated permission requests independent', () => {
    startPermissionDecision('allow-1', 'permission-1', 'allow');
    startPermissionDecision('deny-2', 'permission-2', 'deny');
    markPermissionDecisionUncertain('allow-1');
    expect(permissionDecisions().get('permission-1')?.phase).toBe('uncertain');
    expect(permissionDecisions().get('permission-2')?.phase).toBe('pending');
    retryPermissionDecision('allow-1');
    expect(permissionDecisions().get('permission-1')?.phase).toBe('pending');
  });

  it('unlocks only a definite error for the matching command', () => {
    startPermissionDecision('allow-1', 'permission-1', 'allow');
    failPermissionDecision('other');
    expect(permissionDecisions().has('permission-1')).toBe(true);
    failPermissionDecision('allow-1');
    expect(permissionDecisions().has('permission-1')).toBe(false);
  });

  it('distinguishes a retryable failure from an unknown delivery result', () => {
    startPermissionDecision('allow-1', 'permission-1', 'allow');
    markPermissionDecisionUncertain('allow-1');
    expect(permissionDecisions().get('permission-1')).toMatchObject({ phase: 'uncertain', retryable: false });
    markPermissionDecisionUncertain('allow-1', true);
    expect(permissionDecisions().get('permission-1')).toMatchObject({ phase: 'uncertain', retryable: true });
    retryPermissionDecision('allow-1');
    expect(permissionDecisions().get('permission-1')).toMatchObject({ phase: 'pending', retryable: false });
  });

  it('lets the server projection remove resolved requests authoritatively', () => {
    startPermissionDecision('allow-1', 'permission-1', 'allow');
    startPermissionDecision('deny-2', 'permission-2', 'deny');
    retainProjectedPermissions(new Set(['permission-2']));
    expect([...permissionDecisions().keys()]).toEqual(['permission-2']);
  });

  it('fails closed for an empty permission identity', () => {
    expect(startPermissionDecision('allow-1', '', 'allow')).toBe(false);
    expect(permissionDecisions().size).toBe(0);
  });
});
