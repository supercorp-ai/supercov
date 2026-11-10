// Statement calls (the CallExpression mutator keeps one only when it is the
// statement's sole mutant), methods, arrays, objects, arrows and optional
// chains.
import { log } from "./log";

export function statements(items: string[], options?: { verbose?: boolean; sink?: (line: string) => void }) {
  log();
  log(items);
  log(items.length + 1);
  log("literal");
  log<string>(items.length + 1);
  log(items as unknown, items.length - 1);
  log((value: number) => value);
  log(function named(value) { return value; });
  (log)(items);
  options?.sink?.("optional");
  options?.sink!(items[0]);
  options!.sink!(items[0]);
  items.forEach((item) => log(item));
  items.forEach((item) => {
    log(item);
  });
  require("module-a");
  require.resolve("module-b");
  Symbol("tag");
  Symbol.for("shared");
  new Error("not thrown");
  void log();
  log(/plain/);
  log(/^anchored$/);
  log(new RegExp("\\d+", "g"));
  log(new RegExp("plain"));
  if (items.length) throw new Error("empty");
  if (!items) throw new TypeError();
  if (options) throw items;
  super_like();
}

function super_like() {
  return [Math.min(1, 2), Math.max(3, 4)];
}

export function methods(text: string, values: number[], date: Date) {
  const all = [
    text.charAt(0),
    text.endsWith("a"),
    text.startsWith("b"),
    values.every(Boolean),
    values.some(Boolean),
    values.filter(Boolean),
    values.reverse(),
    values.slice(1),
    values.sort(),
    text.substr(1),
    text.substring(1),
    text.toLocaleLowerCase(),
    text.toLocaleUpperCase(),
    text.toLowerCase(),
    text.toUpperCase(),
    text.trim(),
    text.trimEnd(),
    text.trimStart(),
    Math.min(...values),
    Math.max(...values),
    date.setDate(1),
    date.setTime(2),
    date.setFullYear(3),
    date.setMonth(4),
    date.setHours(5),
    date.setMinutes(6),
    date.setSeconds(7),
    date.setMilliseconds(8),
    date.setUTCDate(9),
    date.setUTCFullYear(10),
    date.setUTCMonth(11),
    date.setUTCHours(12),
    date.setUTCMinutes(13),
    date.setUTCSeconds(14),
    date.setUTCMilliseconds(15),
    date.getUTCDate(),
    text?.trim(),
    text.trim?.(),
    text["trim"](),
    (text.trim)(),
    text.split(",").map((part) => part.trim()).filter((part) => part !== "").join(),
  ];
  return all;
}

export function arrays(size: number) {
  return [
    [],
    [1],
    [, ],
    new Array(size),
    new Array(),
    Array(size),
    Array(),
    Array.from({ length: size }),
    globalThis.Array(),
  ];
}

export function objects(key: string) {
  const empty = {};
  const full = { a: 1, [key]: 2, ...empty };
  const methodsObject = {
    run() {
      return 1;
    },
    get value() {
      return 2;
    },
    set value(next) {},
    async *stream() {
      yield 3;
    },
  };
  const { a = 5, ...rest } = full;
  return [empty, full, methodsObject, a, rest];
}

export function arrows(values: number[]) {
  const id = (x: number) => x;
  const none = () => undefined;
  const nothing = () => {};
  const block = () => {
    return 1;
  };
  const object = () => ({ a: 1 });
  const nested = () => () => 2;
  const asyncArrow = async (x: number) => await Promise.resolve(x);
  const generic = <T,>(x: T): T => x;
  return [id, none, nothing, block, object, nested, asyncArrow, generic, values.map((v) => v)];
}

export function chains(a: any) {
  return [
    a?.b,
    a?.[0],
    a?.(),
    a?.b.c,
    a?.b?.c,
    a.b?.c(),
    a?.b?.(),
    a?.b.c?.d?.(1)?.e,
    (a?.b).c,
    a?.b?.["c"],
  ];
}

export function assigned(target: { run?: () => void; stop?: () => void }) {
  target.run = function () {
    log();
  };
  target.stop = () => {
    log(1 + 1);
  };
}

export function typedArguments(items: string[]) {
  log({ size: 1 } satisfies { size: number });
  log({ size: 2 } satisfies Record<string, number>);
  log({ size: 3 } satisfies object);
  log(<Array<string>>items, 1 + 1);
  log(<string[]>items, 2 + 2);
}
