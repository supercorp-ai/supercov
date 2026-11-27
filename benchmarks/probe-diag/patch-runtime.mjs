// DIAGNOSTIC ONLY. Switches part of a workspace runtime off, for timing alone
// (what it records is no longer right):
//   nohook  the async hook that resets the probe epoch around every async
//           callback is made but never enabled, so probes keep the epoch they
//           first resolved
import { readFileSync, writeFileSync } from 'node:fs';
const [file, what] = process.argv.slice(2);
let text = readFileSync(file, 'utf8');
if (what === 'nohook') {
  const start = text.indexOf('    createHook({\n      // NaN, which no epoch equals');
  const end = text.indexOf('    }).enable();', start);
  if (start < 0 || end < 0) throw new Error('hook not found');
  text = `${text.slice(0, start)}    ({${text.slice(start + '    createHook({'.length, end)}    });${text.slice(end + '    }).enable();'.length)}`;
}
writeFileSync(file, text);
console.log(`patched ${what} in ${file}`);
