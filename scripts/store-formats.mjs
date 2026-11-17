import assert from 'node:assert/strict';

/**
 * The native store formats a release payload admits, read from the SQLite
 * storage source. A Windows checkout carries CRLF line endings, so the text is
 * normalized before the declaration is parsed as lines.
 */
export function readStoreFormats(storage) {
  const source = storage.replace(/\r\n/g, '\n');
  const table = source.match(/^const UPGRADES: \[\(u32, &\[&str\]\); (\d+)\] = \[\n([\s\S]+?)^\];/m);
  assert(table !== null, 'Could not read the native store upgrade table');
  const [, count, upgrades] = table;
  const formats = [...upgrades.matchAll(/^    \(\s*(\d+),/gm)].map(([, format]) => Number(format));
  assert.equal(formats.length, Number(count), 'Could not read every native store upgrade step');
  const declared = source.match(/^pub const FORMAT: u32 = (\d+);$/m);
  assert(declared !== null, 'Could not read the native store format');
  formats.push(Number(declared[1]));
  assert(formats.every((format, index) => Number.isSafeInteger(format) && format > (formats[index - 1] ?? 0)),
    'Native store formats must be strictly increasing');
  return formats;
}
