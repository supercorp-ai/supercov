// What StrykerJS never mutates in TypeScript: types, imports, decorators,
// `declare`, enums, and the string positions it treats as names.
import { readFileSync } from "node:fs";
import type { Stats } from "node:fs";
import * as path from "node:path";
import fs = require("node:fs");
export { join } from "node:path";
export * from "./other";
export type { Dirent } from "node:fs";

declare const injected: string;
declare function declared(value: number): string;
declare module "virtual:config" {
  const value: { mode: "dev" | "prod" };
  export default value;
}
declare global {
  interface Window {
    flag: boolean;
  }
}

enum Color {
  // Stryker disable all
  Red = 1 << 0,
  Green = Red | 2,
  Blue = "blue".length,
}
const enum Size {
  Small = "s",
}

interface Shape {
  kind: "circle" | "square";
  sides?: number;
}

type Pair<T = "x"> = [T, T];
type Flags = { enabled: true; count: 1 };

export namespace Geometry {
  export const unit = 1 + 1;
  export function area(side: number): number {
    return side * side;
  }
}

function decorate(label: string) {
  return (target: unknown) => target;
}

@decorate("entity" + "!")
export class Entity extends Base {
  @decorate("field") name = "unnamed";
  "quoted key" = "quoted value";
  ["computed" + "key"] = 3;
  #secret = "hidden";
  static created = Date.now() + 1;
  declare readonly brand: "entity";
  [key: string]: unknown;

  constructor(private readonly id: string = "none", public size = 1 + 1) {
    super(id);
    this.init();
  }

  @decorate("method")
  describe(this: Entity, verbose = false): string {
    return verbose ? `Entity ${this.id}` : this.id;
  }

  overloaded(value: string): string;
  overloaded(value: number): number;
  overloaded(value: unknown): unknown {
    return value;
  }

  get label(): string {
    return this.name.toUpperCase();
  }

  static {
    Entity.created += 1;
  }
}

export abstract class Base {
  constructor(id?: string) {
    console.log(id);
  }
  abstract render(): string;
  protected init(): void {
    this.ready = true;
  }
  ready = false;
}

class Plain extends Base {
  constructor() {
    super();
  }
}

class WithPrivateField extends Base {
  #count = 0;
  constructor() {
    super();
    this.#count++;
  }
}

class NestedSuper extends Base {
  value = 1;
  constructor() {
    const make = () => super();
    make();
  }
}

class MemberSuper extends Base {
  value = 2;
  constructor() {
    super.init();
  }
}

export function casts(input: unknown, list: number[]) {
  const asserted = input as string;
  const constant = ["a", "b"] as const;
  const angle = <string[]>["c"];
  const satisfied = { mode: "fast", retries: 1 + 1 } satisfies Record<string, unknown>;
  const nonNull = list!.length > 0;
  const generic = new Map<string, number>([["k", 1]]);
  const instantiated = Array.of<number>;
  const typed = (value: string = "default"): value is "default" => value === "default";
  const wrapped = (input as number[]).map((n) => n + 1);
  return [asserted, constant, angle, satisfied, nonNull, generic, instantiated, typed, wrapped];
}
