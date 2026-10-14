// SPDX-License-Identifier: MIT OR Apache-2.0
// Manifest field extraction and Cargo-style runtime range checks for bundling.
// Conservative subset only: plain keys/scalars, raw `::` values, quoted
// `'...'`/`"..."` values, `##` comments, `((...))` blocks (refused for fields).

const fail = message => { throw new Error(`Bundle: ${message}`); };
const FIELDS = ['id', 'name', 'version', 'runtime'];
const REQUIRED = ['id', 'name', 'version'];

// Numeric components are u64-bounded, as in Rust's semver crate.
const tooBig = digits => digits.length > 20
  || (digits.length === 20 && digits > '18446744073709551615');

/** Strict semver subset: no leading zeros in numeric parts; pre/build allowed. */
export function parseVersion(value) {
  const invalid = () => fail(`version: invalid semver '${value}'`);
  if (typeof value !== 'string') invalid();
  const plus = value.indexOf('+');
  const build = plus < 0 ? null : value.slice(plus + 1);
  let core = plus < 0 ? value : value.slice(0, plus);
  const dash = core.indexOf('-');
  const pre = dash < 0 ? null : core.slice(dash + 1);
  core = dash < 0 ? core : core.slice(0, dash);
  const numbers = core.split('.');
  if (numbers.length !== 3 || numbers.some(part => !/^\d+$/.test(part) || /^0\d/.test(part) || tooBig(part))) invalid();
  // Components are BigInt: exact u64 arithmetic, no Number precision loss.
  return Object.freeze({
    major: BigInt(numbers[0]), minor: BigInt(numbers[1]), patch: BigInt(numbers[2]),
    pre: pre === null ? null : identifiers(pre, true, invalid),
    build: build === null ? null : identifiers(build, false, invalid),
  });
}

/** Identifiers allow [0-9A-Za-z-]; numeric prerelease ids ban leading zeros
 *  and stay u64-bounded; build identifiers are arbitrary and uncapped. */
function identifiers(text, strictNumeric, invalid) {
  return Object.freeze(text.split('.').map(id => {
    if (!/^[0-9A-Za-z-]+$/.test(id)) invalid();
    if (strictNumeric && /^\d+$/.test(id)) {
      if (/^0\d/.test(id) || tooBig(id)) invalid();
    }
    return id;
  }));
}

/** -1/0/1 ordering; build metadata ignored; prerelease < release. */
function compareIdentifiers(a, b) {
  const an = /^\d+$/.test(a); const bn = /^\d+$/.test(b);
  if (an && bn) { const x = BigInt(a); const y = BigInt(b); return x < y ? -1 : x > y ? 1 : 0; }
  if (an !== bn) return an ? -1 : 1;
  return a < b ? -1 : a > b ? 1 : 0;
}
function comparePre(a, b) {
  if (a === null && b === null) return 0;
  if (a === null) return 1;
  if (b === null) return -1;
  for (let i = 0; i < Math.max(a.length, b.length); i++) {
    if (i >= a.length) return -1;
    if (i >= b.length) return 1;
    const order = compareIdentifiers(a[i], b[i]);
    if (order) return order;
  }
  return 0;
}
function compare(a, b) {
  for (const key of ['major', 'minor', 'patch']) {
    if (a[key] !== b[key]) return a[key] < b[key] ? -1 : 1;
  }
  return comparePre(a.pre, b.pre);
}

/** Strict range grammar: comma-separated comparators, each an operator from
 *  {=,>,>=,<,<=,^,~} plus a FULL x.y.z version; wildcards, bare and partial
 *  versions, `v`/`latest`, hyphen ranges and `||` are all refused. */
export function parseRuntimeRange(value) {
  const invalid = message => fail(`runtime: ${message}`);
  if (typeof value !== 'string' || value.trim() === '') invalid('invalid range');
  const comparators = [];
  for (const partRaw of value.split(',')) {
    const part = partRaw.trim();
    // Empty parts, leading/trailing commas and `||` are all invalid.
    if (!part || part === '||') invalid('invalid range');
    const op = (part.match(/^(>=|<=|=|\^|~|>|<)/) ?? [''])[0];
    // Whitespace is allowed only between the operator and its version.
    const rest = part.slice(op.length).trim();
    if (rest === '' || /\s/.test(rest)) invalid('invalid range');
    comparators.push(comparator(rest, op, invalid));
  }
  // Keep insertion order; the conjunction order is meaningful in error messages.
  return Object.freeze(comparators);
}

const mkVersion = (nums, pre) => Object.freeze({
  major: nums[0], minor: nums[1], patch: nums[2], pre,
});

function comparator(rest, op, invalid) {
  if (op === '') invalid('bare versions are not supported; use an explicit operator');
  const plus = rest.indexOf('+');
  // Build metadata in a comparator bound is allowed and ignored.
  let main = plus < 0 ? rest : rest.slice(0, plus);
  const build = plus < 0 ? null : rest.slice(plus + 1);
  if (build !== null) {
    if (!/^[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*$/.test(build)) invalid('invalid range');
  }
  let pre = null;
  const dash = main.indexOf('-');
  if (dash >= 0) { pre = main.slice(dash + 1); main = main.slice(0, dash); }
  const segments = main.split('.');
  // Wildcard components are refused; letters inside prerelease or build identifiers are ordinary.
  if (segments.some(n => /^[xX*]$/.test(n))) invalid('wildcards are not supported; use full x.y.z versions');
  // Only full three-component versions; partial bounds and hyphen ranges fail.
  if (segments.length !== 3 || segments.some(n => !/^\d+$/.test(n) || /^0\d/.test(n) || tooBig(n))) invalid('invalid range');
  if (pre !== null) {
    if (!/^[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*$/.test(pre)) invalid('invalid range');
    identifiers(pre, true, invalid);
    if (op === '^' || op === '~') {
      invalid('prerelease bounds are only supported for = and comparison operators');
    }
  }
  const core = segments.map(n => BigInt(n));
  const lo = Object.freeze({ major: core[0], minor: core[1], patch: core[2], pre: pre === null ? null : identifiers(pre, true, invalid) });
  if (op === '^') return Object.freeze({ op, lo, hi: mkVersion(caretHi(core), null) });
  if (op === '~') return Object.freeze({ op, lo, hi: mkVersion([core[0], core[1] + 1n, 0n], null) });
  return Object.freeze({ op, lo, hi: null });
}

function caretHi(c) {
  if (c[0] > 0n) return [c[0] + 1n, 0n, 0n];
  if (c[1] > 0n) return [0n, c[1] + 1n, 0n];
  return [0n, 0n, c[2] + 1n];
}

const sameMmp = (a, b) => a.major === b.major && a.minor === b.minor && a.patch === b.patch;

function comparatorAllows(version, comparator) {
  if (comparator.op === '=') {
    return sameMmp(comparator.lo, version) && comparePre(version.pre, comparator.lo.pre) === 0;
  }
  const order = compare(version, comparator.lo);
  if (comparator.op === '>=') return order >= 0;
  if (comparator.op === '>') return order > 0;
  if (comparator.op === '<') return order < 0;
  if (comparator.op === '<=') return order <= 0;
  // Caret and tilde are inclusive-min/exclusive-max intervals.
  return order >= 0 && compare(version, comparator.hi) < 0;
}

/** Mirrors runtime_requirement_met: any parse failure is simply false. */
export function satisfiesRange(version, range) {
  let parsed; let comparators;
  try {
    parsed = parseVersion(version);
    comparators = parseRuntimeRange(range);
  } catch {
    return false;
  }
  // semver-crate rule: a prerelease version only matches if some >=/>/=
  // comparator with a prerelease on the same major.minor.patch unlocks it.
  if (parsed.pre !== null && !comparators.some(c => (c.op === '>=' || c.op === '>' || c.op === '=')
    && c.lo.pre !== null && sameMmp(c.lo, parsed))) return false;
  for (const comparator of comparators) {
    if (!comparatorAllows(parsed, comparator)) return false;
  }
  return true;
}

/** Extract the top-level id/name/version/runtime fields from alef.ktav text. */
export function readManifestFields(text) {
  const found = {};
  let depth = 0; let i = 0;
  const rootDepth = text.replace(/^[ \r\n]*(?:##[^\n]*(?:\n|$)[ \r\n]*)*/, '').startsWith('{') ? 1 : 0;
  while (i < text.length) {
    if (text.startsWith('##', i)) { i = text.indexOf('\n', i); if (i < 0) break; continue; }
    if (text.startsWith('((', i)) {
      const end = text.indexOf('))', i + 2);
      if (end < 0) break;
      i = end + 2; continue;
    }
    const start = i; let token;
    if (text[i] === '\'' || text[i] === '"' || text[i] === '`') {
      const quote = text[i++]; const begin = i;
      while (i < text.length && text[i] !== quote) { if (text[i] === '\\') i++; i++; }
      token = text.slice(begin, i); i++;
    } else if (/[A-Za-z0-9_.-]/.test(text[i])) {
      while (i < text.length && /[A-Za-z0-9_.-]/.test(text[i])) i++;
      token = text.slice(start, i);
    } else {
      if ('{['.includes(text[i])) depth++;
      if ('}]'.includes(text[i])) depth--;
      i++; continue;
    }
    let at = i; while (at < text.length && /\s/.test(text[at])) at++;
    if (token === undefined || !FIELDS.includes(token) || text[at] !== ':' || depth !== rootDepth) continue;
    if (token in found) fail(`${token}: duplicate field`);
    at++; const raw = text[at] === ':'; if (raw) at++;
    while (at < text.length && /[ \r]/.test(text[at])) at++;
    let value; let end;
    if (text.startsWith('((', at)) fail(`${token}: multiline values unsupported`);
    if (/['"]/.test(text[at] ?? '') && !raw) {
      const quote = text[at]; end = text.indexOf(quote, at + 1);
      if (end < 0) fail(`${token}: unsupported quoted encoding`);
      value = text.slice(at + 1, end); end++;
      if (/[\\\r\n]/.test(value)) fail(`${token}: unsupported quoted encoding`);
    } else if (raw) {
      // Raw values accept any textual content up to end of line.
      end = text.indexOf('\n', at); if (end < 0) end = text.length;
      value = text.slice(at, end).replace(/\r$/, '').trim();
    } else {
      // Plain values end at the first whitespace; same-line trailing text is ambiguous.
      end = at;
      while (end < text.length && !/[\s]/.test(text[end])) end++;
      value = text.slice(at, end);
      let after = end; while (after < text.length && /[ \t\r]/.test(text[after])) after++;
      if (after < text.length && text[after] !== '\n' && !text.startsWith('##', after)) fail(`${token}: ambiguous encoding`);
      end = after;
      if (/^(null|true|false|[+-]?\d+(?:\.\d+)?)$/.test(value)) fail(`${token}: must be a string`);
    }
    // Raw values own the rest of the line; quoted values must end it.
    if (/['"]/.test(text[at] ?? '') && !raw) {
      const newline = text.indexOf('\n', end);
      const tail = text.slice(end, newline < 0 ? text.length : newline).trim();
      if (tail && !tail.startsWith('##')) fail(`${token}: ambiguous encoding`);
    }
    found[token] = value; i = end;
  }
  for (const field of REQUIRED) {
    if (!(field in found)) fail(`missing field ${field}`);
  }
  validateId(found.id);
  validateName(found.name);
  parseVersion(found.version);
  return Object.freeze({ id: found.id, name: found.name, version: found.version, runtime: found.runtime });
}

function validateId(id) {
  if (id.length > 255 || !/^[A-Za-z0-9][A-Za-z0-9.-]*$/.test(id)) fail(`id: invalid identifier '${id}'`);
}
function validateName(name) {
  if (name.length === 0 || name.length > 255) fail('name: invalid display name');
  for (const char of name) {
    const code = char.codePointAt(0);
    if (code < 0x20 || code === 0x7f) fail('name: control characters are forbidden');
  }
}
