import { describe, expect, test } from 'bun:test';
import { ChatActionValidationError } from '@peri-code/sdk';
import { UserError } from 'rivetkit';
import { z } from 'zod';
import { PeriError } from '../src/contracts/errors.js';
import { ActorError } from '../src/errors.js';
import { toUserError } from '../src/rivet/errors.js';

function codeOf(run: () => unknown): string {
  try {
    run();
  } catch (error) {
    expect(error).toBeInstanceOf(UserError);
    return (error as UserError).code as string;
  }
  throw new Error('expected a UserError');
}

describe('rivet error mapping', () => {
  test('keeps ActorError and PeriError codes', () => {
    expect(codeOf(() => toUserError(new ActorError('invalid_input', 'bad input')))).toBe('invalid_input');
    expect(
      codeOf(() => toUserError(new PeriError({ code: 'closed', phase: 'session', message: 'not open' }))),
    ).toBe('closed');
  });

  test('maps SDK action and validation failures to their own codes', () => {
    expect(codeOf(() => toUserError(new ChatActionValidationError('req_1', 'bad action')))).toBe(
      'invalid_action',
    );
    expect(codeOf(() => toUserError(new z.ZodError([])))).toBe('invalid_input');
  });

  test('leaves unknown failures opaque so Rivet reports internal_error', () => {
    const unknown = new Error('internal');
    expect(() => toUserError(unknown)).toThrow(unknown);
  });
});
