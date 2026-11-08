// The thread Node runs module hooks in, where Playwright compiles ES modules
// and writes them into its cache: written whole there too (atomic.mjs says
// why). Supercov's own hooks run in the importing thread where Node can, so
// this goes to that thread on its own.
import { writePlaywrightCacheWhole } from "./atomic.mjs";
writePlaywrightCacheWhole();
