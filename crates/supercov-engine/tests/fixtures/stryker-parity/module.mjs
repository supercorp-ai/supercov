#!/usr/bin/env node
// A plain ES module behind a shebang: loops and labels, classes with private
// members, generators and async code.
import { argv } from "node:process";

const args = argv.slice(2);
const options = { verbose: args.includes("--verbose"), depth: Number(args[0] ?? "3") };

outer: for (let i = 0; i < options.depth; i += 1) {
  for (const arg of args) {
    if (arg === "--stop") break outer;
    if (arg.startsWith("-")) continue outer;
  }
}

for (const key in options) {
  if (!Object.hasOwn(options, key)) continue;
  console.log(`${key}=${options[key]}`);
}

export class Counter {
  #count = 0;
  static #instances = 0;

  constructor(start = 0) {
    this.#count = start;
    Counter.#instances++;
  }

  #bump(by) {
    this.#count += by;
    return this.#count;
  }

  increment() {
    return this.#bump(1);
  }

  get count() {
    return this.#count;
  }

  static get instances() {
    return Counter.#instances;
  }

  *[Symbol.iterator]() {
    for (let n = 0; n < this.#count; n++) yield n;
  }
}

export async function load(urls) {
  const results = await Promise.all(urls.map(async (url) => (await fetch(url)).text()));
  try {
    return results.filter((text) => text.length > 0);
  } catch (error) {
    console.error(error);
    throw new Error(`failed: ${error}`);
  } finally {
    console.log("done");
  }
}

export const meta = import.meta.url + "#main";
export const lazy = () => import("node:path");

function* range(from, to) {
  while (from < to) yield from++;
}

export const total = [...range(0, 3)].reduce((sum, n) => sum + n, 0);
export const spread = { ...options, extra: [...args, "x"] };
export const sequence = (args.length, options.depth);
export const voided = void 0;
export const deleted = delete options.extra;
export const typeOf = typeof options === "object";
export const bigint = 10n * 2n;
export const regex = /[a-z]+\d?/giu.test(args[0] ?? "");
