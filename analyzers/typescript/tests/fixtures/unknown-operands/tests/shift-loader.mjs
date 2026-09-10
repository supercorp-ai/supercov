// Simulate a compiler/loader that moves generated lines without composing maps.
export async function load(url, context, nextLoad) {
  const result = await nextLoad(url, context);
  if (url.endsWith('/tests/runtime.mjs')) {
    return { ...result, source: '\n'.repeat(200) + String(result.source).replace(/\/\/# sourceMappingURL=[^\n]*/g, '') };
  }
  return result;
}
