export function live() { console.log('live'); }
export function beforeReset() { console.log('before reset'); }
export function afterReset() { console.log('after reset'); }
export function wrongReceiver() { console.error('wrong receiver'); }
export function replaced() { console.log('replaced'); }
export function restored() { console.log('restored'); }
export function previous() { console.log('previous'); }
export function beforeSnapshot() { console.log('before snapshot'); }
export function afterSnapshot() { console.log('after snapshot'); }
export function beforeHistory() { console.log('before history'); }
export function afterHistory() { console.log('after history'); }
export function twice() { console.log('twice'); }
export function parameter(value) { console.log(value); }
export function conditional() { console.log('conditional'); }
export function asynchronous() { console.log('async'); }
export function escape(_mock) {}
export function escaped() { console.log('escaped'); }
export function selfCount() { console.log('self count'); }
export function opaque() { const object = {}; console.log('opaque'); return object; }
