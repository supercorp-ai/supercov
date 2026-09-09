export function exact() {
  return 4;
}
export function discarded() {
  return 4;
}
export function coerced() {
  return 4;
}
export function sendStatus(response) {
  response.status(201);
}
export function unchecked() {
  return 4;
}
export function choose(flag) {
  if (flag) return 'yes';
  return 'no';
}
export function overwritten() { return 4; }
export function ignoredCallback() { return 4; }
export function caught() { return 4; }
export function sameLine() { return 4; }
export function retainedAlias() { return 4; }
