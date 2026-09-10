export function throwsValue() { throw new Error('boom'); }
export function noThrowValue() { return 101; }
export async function rejectsValue() { throw new Error('rejected boom'); }
export async function noRejectValue() { return 102; }
export function throwsMessage() { return 'throws diagnostic'; }
export function noThrowMessage() { return 'no-throw diagnostic'; }
export function rejectsMessage() { return 'rejects diagnostic'; }
export function noRejectMessage() { return 'no-reject diagnostic'; }
export function expectedError() { return /^Error: boom$/; }
