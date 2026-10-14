// What the action does that needs no runner: choosing a native package, reading it out of an npm
// archive, and writing the comment. `main.mjs` does the rest.
import { createHash } from 'node:crypto';
import { gunzipSync } from 'node:zlib';

/// The native package for a machine, from `npm/native-targets.json`.
export function nativeTarget(targets, { platform, arch, libc }) {
  const found = targets.find(
    (target) => target.platform === platform && target.arch === arch && (target.libc ?? libc) === libc,
  );
  if (!found) throw new Error(`no Supercov release is built for ${platform}/${arch}${platform === 'linux' ? `/${libc}` : ''}`);
  return found;
}

/// Refuses an archive whose digest is not the one the registry lists for it.
export function verify(archive, integrity) {
  const [algorithm, expected] = integrity.split('-', 2);
  const actual = createHash(algorithm).update(archive).digest('base64');
  if (actual !== expected) throw new Error(`the downloaded archive does not match its ${algorithm} digest`);
}

/// One file out of a gzipped tar archive. npm archives hold plain ustar entries.
export function unpack(archive, name) {
  const tar = gunzipSync(archive);
  for (let offset = 0; offset + 512 <= tar.length; ) {
    const header = tar.subarray(offset, offset + 512);
    if (header.every((byte) => byte === 0)) break;
    const text = (from, to) => header.subarray(from, to).toString('utf8').replace(/\0.*$/s, '');
    const prefix = text(345, 500);
    const path = prefix ? `${prefix}/${text(0, 100)}` : text(0, 100);
    const size = Number.parseInt(text(124, 136).trim() || '0', 8);
    const type = text(156, 157);
    if ((type === '0' || type === '') && path === name) return tar.subarray(offset + 512, offset + 512 + size);
    offset += 512 + Math.ceil(size / 512) * 512;
  }
  throw new Error(`the archive holds no ${name}`);
}

/// Consecutive lines as inclusive ranges.
export function ranges(lines) {
  const out = [];
  for (const line of lines) {
    const last = out.at(-1);
    if (last && last[1] + 1 === line) last[1] = line;
    else out.push([line, line]);
  }
  return out;
}

/// A percentage as the CLI prints it: two decimals, never rounded up to 100.
export function percentage(covered, total) {
  if (!total) return 'n/a';
  const hundredths = Math.floor((covered * 10000) / total);
  return `${(hundredths / 100).toFixed(2)}%`;
}

const count = (covered, total) => `${percentage(covered, total)} (${covered} of ${total})`;

/// The hidden first line that lets a later run find its own comment.
export const marker = (title) => `<!-- supercov-coverage-comment: ${title} -->`;

const inline = (text) => `\`${String(text).replaceAll('`', '')}\``;

/// The comment, which is also the job summary.
///
/// `links` turns a file and a range into an address, or is absent where there is no commit to
/// point at.
export function comment({ title, command, version, failed, summary, patch, patchError, servers = [], floors = [], links, limit = 20 }) {
  const lines = [marker(title), `### ${title}`, ''];
  if (failed !== undefined) {
    lines.push(`The test command ${inline(command)} failed (exit ${failed}), so there is no coverage to report for this commit.`);
  } else {
    measured(lines, { summary, patch, patchError, links, limit });
    // A file is not run at all only where no run measured it: the browser
    // run does not measure the server its tests drove, which has a run of
    // its own.
    const missing = [patch, ...servers.map((server) => server.patch)]
      .filter(Boolean)
      .map((each) => new Set(each.missingFromRun ?? []))
      .reduce((all, each) => (all ? new Set([...all].filter((file) => each.has(file))) : each), undefined);
    if (missing?.size) {
      lines.push('', `Changed, and not run by the test command at all: ${[...missing].map(inline).join(', ')}`);
    }
    for (const server of servers) {
      lines.push('', `#### ${server.language} server`, '');
      measured(lines, { ...server, links, limit });
    }
    for (const floor of floors) lines.push('', `**Below the floor:** ${floor}`);
  }
  lines.push('', `<sub>Measured by <a href="https://supercov.com">Supercov</a> ${version} running ${inline(command)}.</sub>`);
  return `${lines.join('\n')}\n`;
}

/// One run's changed lines, totals and the changed files with lines no test ran.
function measured(lines, { summary, patch, patchError, links, limit }) {
  const coverage = summary.coverage;
  if (patch) {
    lines.push(
      patch.eligible === 0
        ? '**Changed lines:** nothing executable changed.'
        : `**Changed lines: ${count(patch.covered, patch.eligible)}** covered by a test.`,
      '',
    );
  } else if (patchError) {
    lines.push(`Changed lines could not be read: ${patchError}`, '');
  }
  lines.push('| Lines | Branches | MC/DC | Tests |', '| --- | --- | --- | --- |');
  lines.push(
    `| ${count(coverage.lines.covered, coverage.lines.total)} | ${count(coverage.branches.covered, coverage.branches.total)} | ${count(coverage.coveredConditions, coverage.conditions)} | ${summary.tests} |`,
  );
  const uncovered = (patch?.files ?? []).filter((file) => file.uncovered.length > 0);
  if (uncovered.length > 0) {
    lines.push('', '| Changed file | Lines no test ran |', '| --- | --- |');
    for (const file of uncovered.slice(0, limit)) {
      const cells = ranges(file.uncovered).map(([start, end]) => {
        const label = start === end ? `${start}` : `${start}-${end}`;
        const address = links?.(file.file, start, end);
        return address ? `[${label}](${address})` : label;
      });
      lines.push(`| ${inline(file.file)} | ${cells.join(', ')} |`);
    }
    if (uncovered.length > limit) lines.push('', `${uncovered.length - limit} more files have changed lines no test ran.`);
  }
}

/// A workflow command that marks lines in the pull request's diff.
export function annotation(file, start, end) {
  const property = (value) =>
    value.replaceAll('%', '%25').replaceAll('\r', '%0D').replaceAll('\n', '%0A').replaceAll(':', '%3A').replaceAll(',', '%2C');
  const where = start === end ? `line ${start}` : `lines ${start} to ${end}`;
  return `::warning file=${property(file)},line=${start},endLine=${end},title=Not covered::Changed ${where} not run by any test.`;
}

/// An input as the runner passes it: `INPUT_` and the name in capitals.
export function input(environment, name, fallback = '') {
  const value = environment[`INPUT_${name.toUpperCase()}`];
  return value === undefined || value.trim() === '' ? fallback : value.trim();
}
