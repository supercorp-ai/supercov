export function increment(value: number) {
  return value + 1
}

export function wrapper(value: number) {
  return increment(value)
}

export function choose(enabled: boolean) {
  if (enabled) return 'enabled'
  return 'disabled'
}

export function notify(sink: { write: (value: string) => void }) {
  sink.write('hello')
}

export function cleanup(timer: ReturnType<typeof setTimeout>) {
  const pending = setTimeout(() => console.log('expired'), 50)
  clearTimeout(timer)
  return pending
}
