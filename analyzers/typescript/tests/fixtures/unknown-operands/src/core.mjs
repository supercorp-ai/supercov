export function exact() { return 4; }
export function smoke() { return 5; }
export function never() { return 6; }
export function opaque() { return 7; }
export function inactive() { return 8; }
export function failed() { return 9; }
export function mixed() { return 12; }
export function choose(flag) { if (flag) return 10; return 11; }
