const setup = () => { console.log('module initialization'); return () => console.log('after initialization'); };
const emit = setup();
export function effectful() { emit(); }
