// Every operator group, and ConditionalExpression in each position it treats
// differently.
export function arithmetic(a: number, b: number, label: string): number {
  const sum = a + b - 1;
  const scaled = (a * b) / 2 % 7;
  const power = a ** b;
  const shifted = (a << 1) | (b & 3);
  const text = label + a;
  const after = a + label + b;
  const nested = (a + "x") + b;
  const quoted = `${a}` + b;
  const tagged = String.raw`x` + b;
  const negative = -a + +b - ~a;
  return sum + scaled + power + shifted + text.length + after.length + nested.length + quoted.length + tagged.length + negative;
}

export function equality(a: number, b: unknown): boolean[] {
  return [a < 1, a <= 2, a > 3, a >= 4, b == null, b != null, b === a, b !== a, a in {}, b instanceof Date];
}

export function logical(a: boolean, b: boolean | null, c?: string): unknown {
  const either = a || b;
  const both = a && b;
  const fallback = b ?? a;
  const mixed = (a || b) && c;
  const chained = a < 1 || (b && c === "x");
  const deep = (a && b) || (a < 2 && b !== null);
  const coalesced = (a < 1) ?? b;
  return [either, both, fallback, mixed, chained, deep, coalesced];
}

export function assignment(input: number, name: string): unknown {
  let n = input;
  let s = name;
  let maybe: string | undefined;
  n += 1;
  n -= 2;
  n *= 3;
  n /= 4;
  n %= 5;
  n <<= 1;
  n >>= 1;
  n >>>= 1;
  n &= 7;
  n |= 8;
  n ^= 9;
  n **= 2;
  s += "suffix";
  s += `t${n}`;
  s += n;
  maybe ||= "first";
  maybe &&= "second";
  maybe ??= `third`;
  n = 10;
  return [n, s, maybe];
}

export function unaryAndUpdate(values: number[]): number {
  let i = 0;
  let j = values.length;
  i++;
  ++i;
  j--;
  --j;
  const flags = !values.length;
  const twice = !!values[0];
  const kind = typeof values;
  void kind;
  return -i + +j + ~i + (flags ? 1 : 0) + (twice ? 1 : 0) + -1 + -(-1);
}

export function booleans(flag: boolean): boolean[] {
  const always = true;
  const never = false;
  return [always, never, !flag, !true, flag === true];
}

export function conditions(items: number[], limit: number): number {
  let total = 0;
  if (limit > 10) {
    total = 1;
  } else if (!items.length) {
    total = 2;
  } else if (items) {
    total = 3;
  }
  if (limit < 0 && items.length > 0) total++;
  if (limit === 1 || limit === 2) total--;
  for (let i = 0; i < items.length; i++) {
    total += items[i];
  }
  for (;;) {
    if (total > limit) break;
    total++;
  }
  for (let k = 0; ; k++) {
    if (k > 3) break;
  }
  while (total > limit) {
    total -= 1;
  }
  while (items.pop()) {
    total++;
  }
  do {
    total--;
  } while (total > 0 && limit !== 0);
  const picked = limit > 5 ? items[0] : items[1];
  const nestedTernary = limit ? (total >= 3 ? "big" : "small") : "none";
  return total + picked + nestedTernary.length;
}

export function switches(kind: string): number {
  switch (kind) {
    case "a":
    case "b":
      return 1;
    case `c`: {
      const x = 2;
      return x;
    }
    case "d":
      break;
    default:
  }
  switch (kind.length) {
    case 0:
      return 0;
    default:
      return kind.length * 2;
  }
}
