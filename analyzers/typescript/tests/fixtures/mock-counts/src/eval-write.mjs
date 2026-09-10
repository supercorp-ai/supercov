export function producer() { console.log('original producer'); }
export function rewrite() { (eval)("producer = () => console.log('replacement producer')"); }
