import { test } from 'node:test'
import assert from 'node:assert/strict'
import { increment, wrapper, choose, notify } from '../src/core.ts'

test('direct value', () => {
  assert.equal(increment(1), 2)
})

test('through a wrapper', () => {
  assert.equal(wrapper(2), 3)
})

test('enabled outcome', () => {
  assert.equal(choose(true), 'enabled')
})

test('disabled outcome', () => {
  assert.equal(choose(false), 'disabled')
})

test('injected sink', () => {
  const writes: string[] = []
  notify({ write: (value) => writes.push(value) })
  assert.deepEqual(writes, ['hello'])
})

test('execution only', () => {
  increment(3)
})
