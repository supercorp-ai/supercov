import { readFileSync } from 'node:fs';

// The suite reads a file git ignores; it has to reach wherever the tests run.
process.env.FIXTURE_SECRET = readFileSync('.env.test.local', 'utf8').trim();
