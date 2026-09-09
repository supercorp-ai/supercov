export function changed() { console.log('original function'); }
changed = () => console.log('replacement function');
