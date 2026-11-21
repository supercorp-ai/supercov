// StrykerJS's own mutants for JavaScript and TypeScript files, normalized so
// Supercov's enumeration (crates/supercov-engine/src/js_mutants.rs) can be
// compared with them entry by entry.
//
//   node scripts/stryker-parity.mjs [--root DIR] FILE...
//   node scripts/stryker-parity.mjs --write-goldens [--root DIR] FILE...
//
// It runs StrykerJS's instrumenter (only parsing and instrumenting, never a
// test) and prints one JSON entry per mutant:
//
//   { file, mutator, start: [line, column], end: [line, column], replacement, text }
//
// Lines and columns are 1-based, columns in UTF-16 code units, as StrykerJS's
// mutation.json reports them. `text` is StrykerJS's replacement code and
// `replacement` its descriptor (see "Descriptors" below). `file` is relative
// to --root, which defaults to the current directory.
//
// Regex mutants come from weapon-regex and are out of the enumeration's
// scope: they are left out and only counted, as are mutants a
// `// Stryker disable` comment ignores. Both counts go to standard error, and
// into each golden.
//
// --write-goldens writes FILE.stryker.json beside each FILE instead of
// printing.
//
// The instrumenter is @stryker-mutator/instrumenter 10.1.1, resolved from
// STRYKER_INSTRUMENTER_DIR (a directory with it under node_modules) or else
// from the current directory.
//
// Descriptors
// -----------
// A descriptor names a mutant within its span and mutator without printing
// code. Mutant ids are `file:startLine:startCol-endLine:endCol:Mutator:descriptor`.
// The same grammar is implemented in Rust as `descriptor_from_stryker`; keep
// the two in step.
//
//   ArithmeticOperator    the new operator: + - * /
//   EqualityOperator      the new operator: < <= > >= == != === !==
//   LogicalOperator       the new operator: && ||
//   AssignmentOperator    the new operator: += -= *= /= <<= >>= &= |= &&= ||=
//   UpdateOperator        the new operator: ++ --
//   UnaryOperator         the new operator (+ -), or `removed` for ~x -> x
//   BooleanLiteral        the new literal (true false), or `removed` for !x -> x
//   ConditionalExpression true or false (a test, or a for(;;) given a false
//                         test), or `empty` for a switch case emptied
//   StringLiteral         `empty`, or `Stryker was here!` for an empty string
//                         or template
//   MethodExpression      the new method name (startsWith), or `removed`
//                         for x.trim() -> x
//   OptionalChaining      the link that loses its `?`: `.name` (or `.#name`),
//                         `[]` for a computed member, `()` for a call
//   ArrowFunction         undefined
//   BlockStatement        {}
//   ObjectLiteral         {}
//   ArrayDeclaration      the replacement: [] ["Stryker was here"] Array()
//                         Array([]) new Array() new Array([])
//   CallExpression        empty (the statement removed)
//   Regex                 none: not enumerated
import { readFileSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';
import { pathToFileURL } from 'node:url';

export const MUTANT_SET = 'stryker-js@10.1.1';
const INSTRUMENTER = '@stryker-mutator/instrumenter';
const INSTRUMENTER_VERSION = '10.1.1';

const METHOD_REPLACEMENTS = new Map([
  ['charAt', null],
  ['endsWith', 'startsWith'],
  ['every', 'some'],
  ['filter', null],
  ['reverse', null],
  ['slice', null],
  ['sort', null],
  ['substr', null],
  ['substring', null],
  ['toLocaleLowerCase', 'toLocaleUpperCase'],
  ['toLowerCase', 'toUpperCase'],
  ['trim', null],
  ['trimEnd', 'trimStart'],
  ['min', 'max'],
  ['setDate', 'setTime'],
  ['setFullYear', 'setMonth'],
  ['setHours', 'setMinutes'],
  ['setSeconds', 'setMilliseconds'],
  ['setUTCDate', 'setTime'],
  ['setUTCFullYear', 'setUTCMonth'],
  ['setUTCHours', 'setUTCMinutes'],
  ['setUTCSeconds', 'setUTCMilliseconds'],
]);
for (const [name, replacement] of [...METHOD_REPLACEMENTS]) {
  if (replacement && name !== 'setUTCDate') {
    METHOD_REPLACEMENTS.set(replacement, name);
  }
}

function resolveFrom(directory) {
  const require = createRequire(join(resolve(directory), 'noop.js'));
  try {
    const manifest = require.resolve(`${INSTRUMENTER}/package.json`);
    return { require: createRequire(manifest), manifest };
  } catch {
    return undefined;
  }
}

function loadInstrumenter() {
  const directory = process.env.STRYKER_INSTRUMENTER_DIR ?? process.cwd();
  const found = resolveFrom(directory);
  if (!found) {
    const where = process.env.STRYKER_INSTRUMENTER_DIR ? `STRYKER_INSTRUMENTER_DIR (${directory})` : `the current directory (${directory})`;
    console.error(`${INSTRUMENTER} is not installed under ${where}. Install it once, outside the repository, and point at it:

  npm install --prefix /tmp/stryker-tool ${INSTRUMENTER}@${INSTRUMENTER_VERSION}
  STRYKER_INSTRUMENTER_DIR=/tmp/stryker-tool node scripts/stryker-parity.mjs FILE...`);
    process.exit(2);
  }
  const { version } = JSON.parse(readFileSync(found.manifest, 'utf8'));
  if (version !== INSTRUMENTER_VERSION) {
    console.error(`${INSTRUMENTER} ${version} found at ${dirname(found.manifest)}; the mutant set is ${MUTANT_SET}, so ${INSTRUMENTER_VERSION} is needed.`);
    process.exit(2);
  }
  return found;
}

// Line starts as Babel counts lines: \n, \r\n, \r, U+2028 and U+2029 end one.
function lineStarts(text) {
  const starts = [0];
  for (let index = 0; index < text.length; index++) {
    const code = text.charCodeAt(index);
    if (code === 0x0d && text.charCodeAt(index + 1) === 0x0a) {
      index++;
      starts.push(index + 1);
    } else if (code === 0x0a || code === 0x0d || code === 0x2028 || code === 0x2029) {
      starts.push(index + 1);
    }
  }
  return starts;
}

function makeParser(require) {
  const parser = require(require.resolve('@babel/parser'));
  const pluginSets = [['typescript'], ['typescript', 'jsx'], ['jsx']];
  return (code) => {
    for (const plugins of pluginSets) {
      try {
        return parser.parseExpression(code, {
          plugins,
          errorRecovery: true,
          allowAwaitOutsideFunction: true,
          allowReturnOutsideFunction: true,
          allowSuperOutsideMethod: true,
          allowUndeclaredExports: true,
        });
      } catch {
        // Next plugin set.
      }
    }
    return undefined;
  };
}

function unparenthesized(node) {
  let current = node;
  while (current?.type === 'ParenthesizedExpression') {
    current = current.expression;
  }
  return current;
}

/**
 * The descriptor of a StrykerJS mutant (see the grammar at the top of this
 * file), from its mutator name, the original code its span covers, and
 * StrykerJS's replacement code. Undefined for Regex mutants and for anything
 * the grammar does not describe.
 */
// StrykerJS prints the comments a cloned node carries: a `for` or `case`
// replacement can start with the comments above it.
function withoutLeadingComments(code) {
  let text = code.trim();
  for (;;) {
    if (text.startsWith('//')) {
      const end = text.search(/[\n\r\u2028\u2029]/);
      text = end === -1 ? '' : text.slice(end).trimStart();
    } else if (text.startsWith('/*')) {
      const end = text.indexOf('*/');
      text = end === -1 ? '' : text.slice(end + 2).trimStart();
    } else {
      return text;
    }
  }
}

export function descriptorFromStryker(parse, mutator, original, replacement) {
  const text = withoutLeadingComments(replacement);
  switch (mutator) {
    case 'ArithmeticOperator':
    case 'EqualityOperator':
    case 'LogicalOperator': {
      const node = unparenthesized(parse(text));
      return node?.type === 'BinaryExpression' || node?.type === 'LogicalExpression' ? node.operator : undefined;
    }
    case 'AssignmentOperator': {
      const node = unparenthesized(parse(text));
      return node?.type === 'AssignmentExpression' ? node.operator : undefined;
    }
    case 'UpdateOperator': {
      const node = unparenthesized(parse(text));
      return node?.type === 'UpdateExpression' ? node.operator : undefined;
    }
    case 'UnaryOperator':
      return { '+': '-', '-': '+', '~': 'removed' }[original[0]];
    case 'BooleanLiteral':
      if (original.startsWith('!')) return 'removed';
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
      const node = unparenthesized(parse(original.trim()));
      const call = node?.type === 'CallExpression' || node?.type === 'OptionalCallExpression' ? node : undefined;
      const callee = unparenthesized(call?.callee);
      const property = callee?.type === 'MemberExpression' || callee?.type === 'OptionalMemberExpression' ? unparenthesized(callee.property) : undefined;
      if (property?.type !== 'Identifier' || !METHOD_REPLACEMENTS.has(property.name)) return undefined;
      return METHOD_REPLACEMENTS.get(property.name) ?? 'removed';
    }
    case 'OptionalChaining': {
      const node = unparenthesized(parse(original.trim()));
      if (node?.type === 'OptionalCallExpression' && node.optional) return '()';
      if (node?.type === 'OptionalMemberExpression' && node.optional) {
        if (node.computed) return '[]';
        if (node.property.type === 'PrivateName') return `.#${node.property.id.name}`;
        return `.${node.property.name}`;
      }
      return undefined;
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

async function strykerMutants(found, root, files) {
  const { Instrumenter } = await import(pathToFileURL(found.require.resolve(INSTRUMENTER)).href);
  const parse = makeParser(found.require);
  const quiet = () => {};
  const logger = {
    isDebugEnabled: () => false,
    isTraceEnabled: () => false,
    isInfoEnabled: () => false,
    isWarnEnabled: () => true,
    debug: quiet,
    info: quiet,
    trace: quiet,
    warn: console.warn,
    error: console.error,
    fatal: console.error,
  };
  const results = [];
  for (const file of files) {
    const absolute = resolve(file);
    const name = relative(root, absolute).split(sep).join('/');
    const content = readFileSync(absolute, 'utf8');
    let instrumented;
    try {
      instrumented = await new Instrumenter(logger).instrument([{ name, content, mutate: true }], {
        plugins: null,
        ignorers: [],
        excludedMutations: [],
        noHeader: true,
      });
    } catch (error) {
      results.push({ absolute, name, error: String(error.message).split('\n')[0] });
      continue;
    }
    const starts = lineStarts(content);
    const offset = ({ line, column }) => starts[line] + column;
    const mutants = [];
    const skipped = { Regex: 0, ignored: 0 };
    for (const mutant of instrumented.mutants) {
      if (mutant.status === 'Ignored') {
        skipped.ignored++;
        continue;
      }
      if (mutant.mutatorName === 'Regex') {
        skipped.Regex++;
        continue;
      }
      const { start, end } = mutant.location;
      const original = content.slice(offset(start), offset(end));
      const replacement = descriptorFromStryker(parse, mutant.mutatorName, original, mutant.replacement);
      if (replacement === undefined) {
        throw new Error(`${name}:${start.line + 1}:${start.column + 1}: no descriptor for ${mutant.mutatorName} replacing ${JSON.stringify(original)} with ${JSON.stringify(mutant.replacement)}`);
      }
      mutants.push({
        file: name,
        mutator: mutant.mutatorName,
        start: [start.line + 1, start.column + 1],
        end: [end.line + 1, end.column + 1],
        replacement,
        text: mutant.replacement,
      });
    }
    results.push({ absolute, name, mutants, skipped });
  }
  return results;
}

function entryLines(mutants) {
  return mutants.map((mutant) => `  ${JSON.stringify(mutant)}`).join(',\n');
}

async function main() {
  const args = process.argv.slice(2);
  let root = process.cwd();
  let writeGoldens = false;
  const files = [];
  for (let index = 0; index < args.length; index++) {
    const argument = args[index];
    if (argument === '--root') {
      root = resolve(args[++index]);
    } else if (argument === '--write-goldens') {
      writeGoldens = true;
    } else if (argument === '--help' || argument === '-h') {
      console.log('node scripts/stryker-parity.mjs [--write-goldens] [--root DIR] FILE...');
      return;
    } else {
      files.push(isAbsolute(argument) ? argument : resolve(argument));
    }
  }
  if (files.length === 0) {
    console.error('node scripts/stryker-parity.mjs [--write-goldens] [--root DIR] FILE...');
    process.exit(2);
  }
  const found = loadInstrumenter();
  const results = await strykerMutants(found, root, files);
  const all = [];
  let failed = false;
  for (const { absolute, name, mutants, skipped, error } of results) {
    if (error) {
      console.error(`${name}: StrykerJS cannot instrument it: ${error}`);
      failed = true;
      continue;
    }
    console.error(`${name}: ${mutants.length} mutants (${skipped.Regex} Regex and ${skipped.ignored} ignored left out)`);
    if (writeGoldens) {
      const golden = `{\n"mutantSet": ${JSON.stringify(MUTANT_SET)},\n"file": ${JSON.stringify(name)},\n"skipped": ${JSON.stringify(skipped)},\n"mutants": [\n${entryLines(mutants)}\n]\n}\n`;
      writeFileSync(`${absolute}.stryker.json`, golden.replace('[\n\n]', '[]'));
    }
    all.push(...mutants);
  }
  if (!writeGoldens) {
    process.stdout.write(all.length ? `[\n${entryLines(all)}\n]\n` : '[]\n');
  }
  if (failed) {
    process.exitCode = 1;
  }
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  await main();
}
