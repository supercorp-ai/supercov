let client: { ping(): string } | null = null;

export function getClient() {
  if (client) {
    return client;
  }
  client = { ping: () => 'pong' };
  return client;
}

export function label(count: number, strict: boolean) {
  if (count > 1 && strict) {
    return 'many';
  }
  return 'few';
}
