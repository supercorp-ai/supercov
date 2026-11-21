// Supercov's mutants and strong verdicts, and their join with StrykerJS's mutants.
//
// Supercov names a mutant `file:L:C-L:C:Mutator:descriptor` with StrykerJS's 1-based
// positions; the descriptor names it within its span and mutator (grammar in
// scripts/stryker-parity.mjs). A StrykerJS mutant is joined by file, span and mutator,
// then, where that leaves more than one, by replacement code, then by the descriptor
// worked out here from StrykerJS's replacement and the original code. Node only.

import { matchesGlob } from 'node:path';

const STATUS = {
  killed: 'Killed',
  survived: 'Survived',
  timeout: 'Timeout',
  'no-coverage': 'NoCoverage',
  error: 'RuntimeError',
  equivalent: 'Equivalent',
};

const ID = /^(.+?):(\d+):(\d+)-(\d+):(\d+):([A-Za-z]+):(.*)$/;

/** Every mutant object in a `mutants --json` reply (the reply may be wrapped in an envelope). */
function findMutantList(value) {
  if (Array.isArray(value)) {
    if (value.length && value.every((m) => m && typeof m === 'object' && typeof m.id === 'string')) return value;
    for (const item of value) {
      const found = findMutantList(item);
      if (found) return found;
    }
    return null;
  }
  if (value && typeof value === 'object') {
    if (Array.isArray(value.mutants)) {
      const found = findMutantList(value.mutants);
      if (found) return found;
    }
    for (const item of Object.values(value)) {
      const found = findMutantList(item);
      if (found) return found;
    }
  }
  return null;
}

/** Supercov's mutants, inside the project's mutate set, with StrykerJS-style statuses. */
export function readSupercovMutants(text, mutate) {
  let list;
  try {
    list = findMutantList(JSON.parse(text)) ?? [];
  } catch {
    // jsonl: one mutant per line
    list = text.split('\n').filter((line) => line.trim()).map((line) => JSON.parse(line)).filter((m) => typeof m?.id === 'string');
  }
  const inSet = (file) => mutate.include.some((g) => matchesGlob(file, g)) && !mutate.exclude.some((g) => matchesGlob(file, g));
  const mutants = [];
  for (const m of list) {
    const parts = m.id.match(ID);
    if (!parts) continue;
    const [, file, sl, sc, el, ec, mutator, descriptor] = parts;
    if (!inSet(file)) continue;
    const strong = m.strong ?? null;
    mutants.push({
      id: m.id,
      file,
      span: `${sl}:${sc}-${el}:${ec}`,
      mutator,
      descriptor,
      replacement: normalize(m.replacement ?? ''),
      original: m.original ?? null,
      weak: m.status ?? null,
      weakCode: m.weak?.code ?? null,
      strong,
      status: strong ? (STATUS[strong.status] ?? strong.status) : m.status === 'equivalent' ? 'Equivalent' : 'Pending',
      killedBy: strong?.killedBy == null ? [] : [strong.killedBy].flat(),
      reason: strong?.reason ?? m.weak?.reason ?? null,
    });
  }
  return mutants;
}

function normalize(text) {
  let s = text.replace(/\/\*[\s\S]*?\*\/\s*$/g, '').replace(/^(\s*\/\/[^\n]*\n)+/, '').replace(/\s+/g, '');
  while (s.startsWith('(') && s.endsWith(')') && balanced(s.slice(1, -1))) s = s.slice(1, -1);
  return s;
}

function balanced(s) {
  let depth = 0;
  for (const c of s) {
    if (c === '(') depth++;
    else if (c === ')' && --depth < 0) return false;
  }
  return depth === 0;
}

// ---- descriptors of StrykerJS mutants

const METHOD = new Map([
  ['charAt', null], ['endsWith', 'startsWith'], ['every', 'some'], ['filter', null], ['reverse', null],
  ['slice', null], ['sort', null], ['substr', null], ['substring', null], ['toLocaleLowerCase', 'toLocaleUpperCase'],
  ['toLowerCase', 'toUpperCase'], ['trim', null], ['trimEnd', 'trimStart'], ['min', 'max'], ['setDate', 'setTime'],
  ['setFullYear', 'setMonth'], ['setHours', 'setMinutes'], ['setSeconds', 'setMilliseconds'], ['setUTCDate', 'setTime'],
  ['setUTCFullYear', 'setUTCMonth'], ['setUTCHours', 'setUTCMinutes'], ['setUTCSeconds', 'setUTCMilliseconds'],
]);
for (const [name, replacement] of [...METHOD]) if (replacement && name !== 'setUTCDate') METHOD.set(replacement, name);

const OPERATORS = ['>>>=', '===', '!==', '**=', '&&=', '||=', '??=', '<<=', '>>=', '>>>', '=>', '==', '!=', '<=', '>=', '&&', '||', '??', '+=', '-=', '*=', '/=', '%=', '&=', '|=', '^=', '++', '--', '**', '<<', '>>', '?.', '<', '>', '+', '-', '*', '/', '%', '=', '&', '|', '^', '!', '~', '?', ':'];
const PRECEDENCE = { '??': 1, '||': 1, '&&': 2, '|': 3, '^': 4, '&': 5, '==': 6, '!=': 6, '===': 6, '!==': 6, '<': 7, '>': 7, '<=': 7, '>=': 7, '<<': 8, '>>': 8, '>>>': 8, '+': 9, '-': 9, '*': 10, '/': 10, '%': 10, '**': 11 };
const ASSIGN = new Set(['=', '+=', '-=', '*=', '/=', '%=', '<<=', '>>=', '>>>=', '&=', '|=', '^=', '&&=', '||=', '??=', '**=']);

/** Top-level tokens of an expression: operators at depth 0 with their position, strings skipped. */
function topLevelTokens(text) {
  const tokens = [];
  let depth = 0;
  let previous = 'start';
  for (let i = 0; i < text.length; ) {
    const c = text[i];
    if (c === '"' || c === "'" || c === '`') {
      let j = i + 1;
      while (j < text.length && text[j] !== c) j += text[j] === '\\' ? 2 : 1;
      i = j + 1;
      previous = 'value';
      continue;
    }
    if ('([{'.includes(c)) { depth++; i++; previous = 'open'; continue; }
    if (')]}'.includes(c)) { depth--; i++; previous = 'value'; continue; }
    if (/\s/.test(c)) { i++; continue; }
    const op = OPERATORS.find((o) => text.startsWith(o, i));
    if (op) {
      if (depth === 0) tokens.push({ op, index: i, unary: previous !== 'value' });
      i += op.length;
      previous = op === '++' || op === '--' ? previous : 'op';
      continue;
    }
    i++;
    previous = 'value';
  }
  return tokens;
}

function rootBinaryOperator(text) {
  let best = null;
  for (const t of topLevelTokens(text)) {
    if (t.unary || !(t.op in PRECEDENCE)) continue;
    if (!best || PRECEDENCE[t.op] <= PRECEDENCE[best.op]) best = t;
  }
  return best?.op;
}

/** The descriptor Supercov would give a StrykerJS mutant (scripts/stryker-parity.mjs's grammar). */
export function strykerDescriptor(mutator, original, replacement) {
  const text = replacement.replace(/^(\s*\/\/[^\n]*\n|\s*\/\*[\s\S]*?\*\/)+/, '').trim();
  const orig = original.trim();
  switch (mutator) {
    case 'ArithmeticOperator':
    case 'EqualityOperator':
    case 'LogicalOperator':
      return rootBinaryOperator(text.replace(/^\((.*)\)$/s, (m, inner) => (balanced(inner) ? inner : m)));
    case 'AssignmentOperator':
      return topLevelTokens(text).find((t) => ASSIGN.has(t.op))?.op;
    case 'UpdateOperator':
      return text.includes('++') ? '++' : text.includes('--') ? '--' : undefined;
    case 'UnaryOperator':
      return { '+': '-', '-': '+', '~': 'removed' }[orig[0]];
    case 'BooleanLiteral':
      if (orig.startsWith('!')) return 'removed';
      return text === 'true' || text === 'false' ? text : undefined;
    case 'ConditionalExpression':
      if (text === 'true' || text === 'false') return text;
      if (/^for\s*\(/.test(text)) return 'false';
      if (/^(case\b|default\s*:)/.test(text)) return 'empty';
      return undefined;
    case 'StringLiteral':
      if (text === '""' || text === "''" || text === '``') return 'empty';
      if (text.slice(1, -1) === 'Stryker was here!') return 'Stryker was here!';
      return undefined;
    case 'MethodExpression': {
      // The callee's property: the last `.name(` at depth 0 of the original call.
      let name;
      let depth = 0;
      for (let i = 0; i < orig.length; i++) {
        const c = orig[i];
        if ('([{'.includes(c)) depth++;
        else if (')]}'.includes(c)) depth--;
        else if (c === '.' && depth === 0) {
          const m = orig.slice(i).match(/^\.\s*([A-Za-z_$][\w$]*)\s*\(/);
          if (m) name = m[1];
        }
      }
      if (!name || !METHOD.has(name)) return undefined;
      return METHOD.get(name) ?? 'removed';
    }
    case 'OptionalChaining': {
      // The span is the optional member or call: its own link is the last `?.` at depth 0.
      let last;
      let depth = 0;
      for (let i = 0; i < orig.length; i++) {
        const c = orig[i];
        if (depth === 0 && orig.startsWith('?.', i)) last = i;
        if ('([{'.includes(c)) depth++;
        else if (')]}'.includes(c)) depth--;
      }
      if (last == null) return undefined;
      const after = orig.slice(last + 2);
      if (after.startsWith('(')) return '()';
      if (after.startsWith('[')) return '[]';
      const m = after.match(/^(#?[A-Za-z_$][\w$]*)/);
      return m ? `.${m[1]}` : undefined;
    }
    case 'ArrowFunction':
      return text.replace(/\s+/g, '') === '()=>undefined' ? 'undefined' : undefined;
    case 'BlockStatement':
    case 'ObjectLiteral':
      return text.replace(/\s+/g, '') === '{}' ? '{}' : undefined;
    case 'ArrayDeclaration': {
      const compact = text.replace(/\s+/g, ' ').replace(/\[ /g, '[').replace(/ \]/g, ']');
      return ['[]', '["Stryker was here"]', 'Array()', 'Array([])', 'new Array()', 'new Array([])'].includes(compact) ? compact : undefined;
    }
    case 'CallExpression':
      return text === ';' || text === '' ? 'empty' : undefined;
    default:
      return undefined;
  }
}

/** The original code of a StrykerJS mutant, from its report's source and 1-based location. */
export function originalCode(source, location) {
  if (!source) return '';
  const lines = source.split('\n');
  const { start, end } = location;
  if (start.line === end.line) return lines[start.line - 1]?.slice(start.column - 1, end.column - 1) ?? '';
  const parts = [lines[start.line - 1]?.slice(start.column - 1) ?? ''];
  for (let l = start.line; l < end.line - 1; l++) parts.push(lines[l] ?? '');
  parts.push(lines[end.line - 1]?.slice(0, end.column - 1) ?? '');
  return parts.join('\n');
}

/**
 * Joins Supercov's mutants to StrykerJS's (each StrykerJS mutant: file, span, mutator,
 * replacement (normalized), rawReplacement, original). Returns pairs and the unmatched.
 */
export function joinMutants(supercovMutants, strykerMutants) {
  const bySpan = new Map();
  for (const m of strykerMutants) {
    const key = `${m.file}|${m.span}|${m.mutator}`;
    (bySpan.get(key) ?? bySpan.set(key, []).get(key)).push(m);
  }
  const pairs = [];
  const used = new Set();
  const unmatched = [];
  const ambiguous = [];
  for (const s of supercovMutants) {
    const candidates = (bySpan.get(`${s.file}|${s.span}|${s.mutator}`) ?? []).filter((m) => !used.has(m));
    let match = null;
    if (candidates.length === 1) match = candidates[0];
    else if (candidates.length > 1) {
      const byCode = candidates.filter((m) => m.replacement === s.replacement);
      const byDescriptor = candidates.filter((m) => strykerDescriptor(m.mutator, m.original ?? '', m.rawReplacement ?? '') === s.descriptor);
      if (byCode.length === 1) match = byCode[0];
      else if (byDescriptor.length === 1) match = byDescriptor[0];
      else ambiguous.push(s.id);
    }
    if (match) {
      used.add(match);
      pairs.push([s, match]);
    } else unmatched.push(s);
  }
  return { pairs, supercovOnly: unmatched, strykerOnly: strykerMutants.filter((m) => !used.has(m)), ambiguous };
}
