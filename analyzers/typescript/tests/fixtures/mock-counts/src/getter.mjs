const configuration = { get info() { return () => console.log('getter'); } };
export function getterLogger() { return configuration; }
