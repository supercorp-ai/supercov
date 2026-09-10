export function syncNormal() { return 101; }
export async function asyncNormal() { return 102; }
export function syncThrow() { throw Error('boom'); }
export async function asyncThrow() { throw Error('rejected boom'); }
export function throwFactory() { return () => { throw Error('callback boom'); }; }
export function normalFactory() { return () => 103; }
export async function fulfilledPromise() { return 104; }
export async function rejectedPromise() { throw Error('promise boom'); }
