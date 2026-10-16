// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { stripTypeScriptTypes } from 'node:module';
import { dirname, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { rewriteDeclarationSpecifiers } from '../../build.mjs';

const pkgDir = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const pkgUrl = pkgDir.replaceAll('\\', '/');
const manifest = JSON.parse(readFileSync(resolve(pkgDir, 'package.json'), 'utf8'));
const exportsMap = manifest.exports;
// Sources live in the "alef-sources" manifest field (monorepo-only; npm ignores it and nothing
// resolves through it). The guards above keep checking the sources, the ones below the dist.
const sources = manifest['alef-sources'];
const rel = p => p.replaceAll('\\', '/').slice(pkgUrl.length + 1);
const under = (file, dir) => file.startsWith(`${pkgUrl}/${dir}/`);

// Static import specifiers: `... from 'x'` and side-effect `import 'x'`; both span lines.
const FROM = /(?:^|\n)\s*(?:import|export)\s[\s\S]*?from\s*['"]([^'"]+)['"]/g;
const SIDE = /(?:^|\n)\s*import\s*['"]([^'"]+)['"]/g;

// Explicit edges between modules; everything else outside src/core/ is a violation.
const EDGES = [
  // client and server share one transport and body handling
  { from: 'src/net/http.ts', to: 'src/net/http-server.ts' },
  { from: 'src/net/http.ts', to: 'src/net/body.ts' },
  // the server reads bodies and upgrades to WebSocket connections
  { from: 'src/net/http-server.ts', to: 'src/net/body.ts' },
  { from: 'src/net/http-server.ts', to: 'src/net/websocket-connection.ts' },
  // the WebSocket client and server build on the HTTP transport
  { from: 'src/net/websocket.ts', to: 'src/net/http.ts' },
  { from: 'src/net/websocket.ts', to: 'src/net/http-server.ts' },
  { from: 'src/net/websocket.ts', to: 'src/net/websocket-connection.ts' },
  // a menu attaches to a window
  { from: 'src/desktop/menu.ts', to: 'src/desktop/window.ts', typeOnly: true },
];


function isAllowed(from, to) {
  if (typeEntries.has(from)) return to === typeEntries.get(from);
  if (under(to, 'types')) return true;
  if (under(to, 'src/core')) return true;
  if (under(from, 'src/core') && dirname(from) === dirname(to)) return true;
  if (from === `${pkgUrl}/src/index.ts`) return true;
  return EDGES.some(edge => from === `${pkgUrl}/${edge.from}` && to === `${pkgUrl}/${edge.to}`);
}

function typeOnlyViolations(sourceFile, text) {
  const edge = EDGES.find(e => sourceFile === `${pkgUrl}/${e.from}` && e.typeOnly);
  if (!edge) return [];
  const target = resolve(pkgUrl, edge.to);
  const statements = [...text.matchAll(FROM)].filter(match => resolve(dirname(sourceFile), match[1]) === target);
  if (statements.length === 0) return [`"${rel(sourceFile)}" must keep the type-only import of "${edge.to}"`];
  return statements
    .filter(match => !/^\s*import\s+type\s/.test(match[0]))
    .map(match => `"${rel(sourceFile)}" must import "${edge.to}" only with "import type": ${match[0].trim()}`);
}

function checkEntry(entryFile) {
  const violations = [];
  const visited = new Set();
  const worklist = [entryFile];
  while (worklist.length > 0) {
    const sourceFile = worklist.pop().replaceAll('\\', '/');
    if (visited.has(sourceFile)) continue;
    visited.add(sourceFile);
    if (under(sourceFile, 'types')) continue; // the leaf of types is never walked
    const text = readFileSync(sourceFile, 'utf8');
    const specifiers = [...text.matchAll(FROM), ...text.matchAll(SIDE)].map(match => match[1]);
    for (const specifier of specifiers) {
      if (!specifier.startsWith('.')) {
        violations.push(`"${rel(sourceFile)}" must not import "${specifier}"`);
        continue;
      }
      let target = fileURLToPath(new URL(specifier, pathToFileURL(sourceFile))).replaceAll('\\', '/');
      if (!target.endsWith('.ts')) target += '.ts';
      if (!isAllowed(sourceFile, target)) {
        violations.push(`"${rel(sourceFile)}" must not import "${rel(target)}"`);
      }
      worklist.push(target);
    }
    violations.push(...typeOnlyViolations(sourceFile, text));
  }
  return violations;
}

// Type entries: normalized path of "<subpath>/types" target -> normalized path of the code entry target.
// Keyed in the same representation isAllowed()/checkEntry() use, not as file URLs.
const typeEntries = new Map(
  Object.keys(exportsMap)
    .filter(subpath => subpath.endsWith('/types') && subpath !== './types')
    .map(subpath => [
      resolve(pkgDir, sources[subpath]).replaceAll('\\', '/'),
      resolve(pkgDir, sources[subpath.slice(0, -'/types'.length)]).replaceAll('\\', '/'),
    ]),
);

// A runtime entry must not be imported through `./types`: the test checks that directory statically.
const runtimeEntries = Object.keys(exportsMap).filter(subpath => !under(resolve(pkgDir, sources[subpath]).replaceAll('\\', '/'), 'types'));

test('every subpath export of @alef-runtime/api points at an existing source file', () => {
  const keys = Object.keys(exportsMap);
  assert.ok(keys.length >= 25, `expected at least 25 subpath exports, got ${keys.length}`);
  for (const [subpath, target] of Object.entries(exportsMap)) {
    const file = resolve(pkgDir, sources[subpath]);
    assert.ok(readFileSync(file), `export "${subpath}" points at missing source ${target}`);
  }
});

test('the AI segment exports only its allow-listed values', async () => {
  const segment = await import('../../src/system/ai.ts');
  assert.deepEqual(Object.keys(segment).sort(), ['ai', 'chat', 'request', 'roles', 'session']);
  assert.deepEqual(Object.keys(segment.ai).sort(), ['chat', 'request', 'roles', 'session', 'tools']);
});

// Split masked source into top-level statements: { text, line } (1-based line of the statement head).
function topLevelStatements(masked) {
  const statements = [];
  let start = 0;
  let depth = 0;
  let head = ''; // text before the first `{` of the current statement
  let sawBrace = false;
  let i = 0;
  const end = endPos => {
    const text = masked.slice(start, endPos).trim();
    if (text) statements.push({ text, line: masked.slice(0, start).split('\n').length, start, end: endPos });
    start = endPos;
    head = '';
    sawBrace = false;
  };
  while (i < masked.length) {
    const ch = masked[i];
    if (ch === '{' && !sawBrace) {
      sawBrace = true;
      head = masked.slice(start, i);
    }
    if (ch === '{' || ch === '(' || ch === '[') depth++;
    else if (ch === '}' || ch === ')' || ch === ']') {
      depth--;
      if (depth < 0) depth = 0; // a stray closer is its own (flagged) statement
      if (ch === '}' && depth === 0 && head !== '' && bracedDeclarationHead(head)) {
        let j = i + 1; // consume whitespace and an optional trailing `;` (e.g. left by a stripped `as const`)
        while (masked[j] === ' ') j++;
        // A binding goes on after its object when an operator follows: `type A = { .. } | { .. }`.
        if (bindingHead(head) && /^(?:[|&.[,?]|as\b|satisfies\b)/.test(masked.slice(j))) {
          i++;
          continue;
        }
        if (masked[j] === ';') i = j;
        end(i + 1);
        i++;
        continue;
      }
    } else if (ch === ';' && depth === 0) {
      end(i + 1);
      i++;
      continue;
    } else if (ch === '\n' && depth === 0) {
      if (!masked.slice(start, i).trim()) { start = i + 1; i++; continue; } // keep the head line exact across blank lines
      const next = masked.slice(i + 1).replace(/^ +/, '');
      const joins = /^[.?:,]|^&&|^\|\||^\?\?|^=>/.test(next); // a line opening with an operator cannot start a statement
      if (!continuesOnNextLine(masked.slice(start, i)) && !joins) { end(i); start = i + 1; } // the next statement begins after the newline
      i++;
      continue;
    }
    i++;
  }
  end(masked.length);
  return statements;
}

// A `}` closing one of these heads ends the statement even without a `;`.
function bracedDeclarationHead(head) {
  const h = head.trim();
  if (/^(?:export\s+|declare\s+|default\s+|async\s+)*(?:function|class|interface|enum|namespace)\b/.test(h)) return true;
  return bindingHead(head);
}

// Object-typed binding: `const|let|var X = {`, `export ... = {`, `type X = {`.
function bindingHead(head) {
  return /^(?:export\s+)?(?:const|let|var|type)\b[^=]*=\s*$/.test(head.trim());
}

// A line keeps the statement going only with an explicit operator tail; when unsure, split.
// A wrong split fails the allow-lists, which is the safe direction.
const BINDING_HEAD = /^(?:export\s+)?(?:const|let|var)\b/;
function continuesOnNextLine(text) {
  const s = text.replace(/\s+$/, '');
  if (!s) return true;
  if (/[.]$/.test(s)) return true;
  const op = /(?:&&|\|\||\?\?|=>|===|!==|==|!=|<=|>=|\*\*|<<|>>|[=<>+\-*/%|&^?:,])$/.exec(s);
  if (!op) return false;
  if (op[0] === '+' || op[0] === '-') return BINDING_HEAD.test(text.trimStart()); // ambiguous under ASI
  return true;
}

// Every statement of a type-only file must be a type statement. `generated` additionally allows
// `export * from` for the generated DTO re-export barrel; type-entries/ stays strict without it.
function typeStatementTextViolations(label, text, generated = false) {
  const violations = [];
  for (const { text: s } of topLevelStatements(mask(text))) {
    if (/^(?:import\s+type\b|export\s+type\b|interface\b|type\s)/.test(s)) continue;
    if (generated && /^export\s+\*\s+from\b/.test(s)) continue;
    const braces = /^export\s*\{([^}]*)\}/.exec(s); // every specifier of a re-export must be `type`
    if (braces && braces[1].split(',').every(sp => /^\s*type\b/.test(sp))) continue;
    violations.push(`"${label}" has a statement that is not a type: ${s}`);
  }
  return violations;
}

function typeStatementViolations(file, label, generated = false) {
  return typeStatementTextViolations(label, readFileSync(file, 'utf8'), generated);
}

test('the generated types directory declares and re-exports only types', () => {
  const dir = resolve(pkgDir, 'types');
  const files = readdirSync(dir, { recursive: true }).map(String).filter(name => name.endsWith('.ts'));
  assert.ok(files.length > 0);
  const violations = files.flatMap(name => typeStatementViolations(resolve(dir, name), `types/${name}`, true));
  assert.deepEqual(violations, [], violations.join('\n'));
});

test('a value import beside the type-only edge is caught', () => {
  const menu = `${pkgUrl}/src/desktop/menu.ts`;
  assert.deepEqual(typeOnlyViolations(menu, "import type { AppWindow } from './window.ts';\n"), []);
  assert.equal(typeOnlyViolations(menu, "import type { AppWindow } from './window.ts';\nimport { nativeWindow } from './window.ts';\n").length, 1);
});

test('the import parser reads import and export statements that span several lines', () => {
  const sample = 'import {\n  fs,\n} from \'./desktop/window.ts\';\n';
  assert.equal([...sample.matchAll(FROM)][0][1], './desktop/window.ts');
  const specifiers = [...readFileSync(resolve(pkgDir, 'src/index.ts'), 'utf8').matchAll(FROM)].map(match => match[1]);
  assert.ok(specifiers.includes('./desktop/window.ts'), 'the multiline exports of src/index.ts must be parsed');
});

test('the import graph of every subpath stays inside core, its own module and the listed edges', () => {
  const all = [];
  for (const subpath of runtimeEntries) {
    all.push(...checkEntry(resolve(pkgDir, sources[subpath])));
  }
  assert.deepEqual(all, [], 'import graph violations:\n' + all.join('\n'));
});

test('importing any subpath entry issues no runtime call', { timeout: 300000 }, () => {
  // The child sees its entry and helper as file:// URLs through the environment, not argv.
  const childScript = `
    const { installRuntime } = await import(process.env.ALEF_HELPER);
    const runtime = installRuntime();
    let listeners = 0;
    const wrap = target => {
      const real = target.addEventListener?.bind(target);
      target.addEventListener = (...args) => {
        listeners += 1;
        return real?.(...args);
      };
    };
    wrap(globalThis);
    for (const key of ['window', 'document']) {
      if (!globalThis[key]) globalThis[key] = {};
      wrap(globalThis[key]);
    }
    let timers = 0;
    const realTimeout = globalThis.setTimeout.bind(globalThis);
    const realInterval = globalThis.setInterval.bind(globalThis);
    globalThis.setTimeout = (...args) => {
      timers += 1;
      return realTimeout(...args);
    };
    globalThis.setInterval = (...args) => {
      timers += 1;
      return realInterval(...args);
    };
    await import(process.env.ALEF_ENTRY);
    console.log(JSON.stringify({ requests: runtime.requests.length, listeners, timers }));
  `;
  for (const subpath of runtimeEntries) {
    const run = spawnSync(process.execPath, ['--experimental-strip-types', '--input-type=module', '-e', childScript], {
      timeout: 20000,
      encoding: 'utf8',
      env: {
        ...process.env,
        ALEF_ENTRY: pathToFileURL(resolve(pkgDir, sources[subpath])).href,
        ALEF_HELPER: pathToFileURL(resolve(pkgDir, 'test/fake-runtime.mjs')).href,
      },
    });
    assert.equal(run.status, 0, `subpath "${subpath}" failed to import: ${run.stderr}`);
    const counts = JSON.parse(run.stdout.trim());
    assert.equal(counts.requests, 0, `subpath "${subpath}" issued ${counts.requests} runtime call(s) at import time`);
    assert.equal(counts.listeners, 0, `subpath "${subpath}" registered ${counts.listeners} listener(s) at import time`);
    assert.equal(counts.timers, 0, `subpath "${subpath}" scheduled ${counts.timers} timer(s) at import time`);
  }
});

test('every code subpath has exactly one uniform type entry under type_entries', () => {
  const codeEntries = Object.entries(exportsMap).filter(([subpath]) => subpath !== '.' && subpath !== './types' && !subpath.endsWith('/types'));
  assert.equal(codeEntries.length, 24, `expected 24 code subpaths, got ${codeEntries.length}`);
  const violations = [];
  for (const [subpath] of codeEntries) {
    const typesSubpath = `${subpath}/types`;
    const typesTarget = exportsMap[typesSubpath];
    if (typesTarget === undefined) {
      violations.push(`export "${subpath}" has no "${typesSubpath}" entry`);
      continue;
    }
    const file = resolve(pkgDir, sources[typesSubpath]).replaceAll('\\', '/');
    if (!under(file, 'type-entries')) violations.push(`"${typesSubpath}" must point into type-entries/, got ${sources[typesSubpath]}`);
    violations.push(...typeStatementViolations(file, typesSubpath));
  }
  assert.deepEqual(violations, [], violations.join('\n'));
});

// A '/' after one of these significant characters or keywords starts a regex, not a division.
// A `"` at the end of a masked span is the terminal token of a string/template literal.
const REGEX_PRECEDERS = new Set(['=', '(', ',', ':', '[', '!', '&', '|', '?', '{', '}', ';', '+', '-', '*', '%', '^', '~', '>', '"']);
const REGEX_PRECEDING_WORDS = new Set(['return', 'typeof', 'case', 'in', 'of', 'new', 'delete', 'void', 'do', 'else', 'yield', 'await']);

// Blank out comments, strings and template literals (interpolations included), preserving newlines.
// Each literal keeps one placeholder token `"` at its closing quote, so a line ending in a string
// no longer looks like it ends in an operator (ASI decisions read the real terminal token).
function mask(src) {
  let out = '';
  let templateDepths = [];
  let depth = 0;
  let i = 0;
  const blank = ch => (ch === '\n' ? '\n' : ' ');
  const prevSignificant = () => {
    for (let j = out.length - 1; j >= 0; j--) {
      const ch = out[j];
      if (ch !== ' ') return ch;
    }
    return '';
  };
  const prevWord = () => {
    const match = /([A-Za-z_$][$A-Za-z0-9_]*)\s*$/.exec(out);
    return match ? match[1] : '';
  };
  while (i < src.length) {
    const ch = src[i];
    const next = src[i + 1];
    if (ch === '/' && next === '*') {
      const end = src.indexOf('*/', i + 2);
      const stop = end === -1 ? src.length : end + 2;
      while (i < stop) out += blank(src[i++]);
      continue;
    }
    if (ch === '/' && next === '/') {
      const end = src.indexOf('\n', i);
      const stop = end === -1 ? src.length : end;
      while (i < stop) out += blank(src[i++]);
      continue;
    }
    if (ch === '"' || ch === "'" || ch === '`' && templateDepths.length === 0) {
      const quote = ch;
      out += blank(src[i++]);
      while (i < src.length) {
        if (src[i] === '\\') {
          out += blank(src[i++]);
          if (i < src.length) out += blank(src[i++]);
          continue;
        }
        if (src[i] === quote) {
          out += '"'; // placeholder terminal token of the literal
          i++;
          break;
        }
        if (quote === '`' && src[i] === '$' && src[i + 1] === '{') {
          out += blank(src[i++]);
          out += blank(src[i++]);
          templateDepths.push(depth);
          break;
        }
        out += blank(src[i] === '\n' ? '\n' : ' ');
        i++;
      }
      continue;
    }
    if (ch === '`') {
      templateDepths.push(depth);
      out += blank(src[i++]);
      continue;
    }
    if (ch === '}' && templateDepths.length > 0 && depth === templateDepths[templateDepths.length - 1]) {
      templateDepths.pop();
      out += blank(src[i++]);
      // resume the enclosing template literal
      const quote = '`';
      while (i < src.length) {
        if (src[i] === '\\') {
          out += blank(src[i++]);
          if (i < src.length) out += blank(src[i++]);
          continue;
        }
        if (src[i] === quote) {
          out += '"'; // placeholder terminal token of the template
          i++;
          break;
        }
        if (src[i] === '$' && src[i + 1] === '{') {
          out += blank(src[i++]);
          out += blank(src[i++]);
          templateDepths.push(depth);
          break;
        }
        out += blank(src[i] === '\n' ? '\n' : ' ');
        i++;
      }
      continue;
    }
    if (ch === '/' && templateDepths.length === 0) {
      const prev = prevSignificant();
      const word = prevWord();
      const regexStart = prev === '' || REGEX_PRECEDERS.has(prev) || REGEX_PRECEDING_WORDS.has(word);
      if (regexStart) {
        out += blank(src[i++]);
        while (i < src.length && src[i] !== '/' && src[i] !== '\n') {
          if (src[i] === '\\') {
            out += blank(src[i++]);
            if (i < src.length) out += blank(src[i++]);
            continue;
          }
          out += ' ';
          i++;
        }
        if (src[i] === '/') out += blank(src[i++]);
        continue;
      }
    }
    if ('{(['.includes(ch)) depth++;
    else if ('})]'.includes(ch)) depth--;
    out += ch;
    i++;
  }
  return out;
}

// Constructors that are genuinely pure at module level; nothing else may be constructed at top level.
const PURE_CONSTRUCTORS = new Set([
  'TextEncoder', // stateless codec, allocates nothing external
  'TextDecoder', // stateless codec, allocates nothing external
  'Map', // empty in-memory collection, no I/O
  'Set', // empty in-memory collection, no I/O
  'Store', // constructor only assigns `this.area`
]);

// An initializer must be free of eager calls and constructions; lazy arrow bodies are skipped
// (and scanning resumes after them) inside hasCallOrNew.
function initializerIsPure(statement) {
  const masked = mask(statement);
  let at = masked.indexOf('='); // first binding `=` (not part of `=>`/`==`; destructuring defaults do not appear at top level)
  while (at !== -1 && (masked[at + 1] === '=' || masked[at + 1] === '>' || masked[at - 1] === '=' || masked[at - 1] === '!' || masked[at - 1] === '<' || masked[at - 1] === '>')) at = masked.indexOf('=', at + 1);
  if (at === -1) return true; // a bare `let p;` declares nothing
  const init = masked.slice(at + 1).replace(/;+\s*$/, '');
  return !hasCallOrNew(init);
}

// A call or a `new` outside the allow-list in the eager part of an initializer is a side effect.
// `text` must already be masked. Lazy bodies are skipped, then scanning resumes: a braced body via
// the per-brace fnBody stack, an unbraced arrow body until a `,`/`)`/`]`/`}` at the arrow's depth.
function hasCallOrNew(text) {
  const masked = text;
  const fnBody = []; // per open brace: true when the body belongs to a function/class/braced arrow (lazy)
  let depth = 0;
  let skip = -1; // depth of the arrow operator whose unbraced body is being skipped, -1 when not
  for (let i = 0; i < masked.length; i++) {
    const ch = masked[i];
    if (skip !== -1) {
      if ((ch === ',' || ch === ')' || ch === ']' || ch === '}') && depth === skip) skip = -1;
      else {
        if (ch === '(' || ch === '[' || ch === '{') depth++;
        else if (ch === ')' || ch === ']' || ch === '}') depth--;
      }
      continue;
    }
    if (ch === '{') {
      depth++;
      const head = masked.slice(0, i).trimEnd().slice(-120);
      fnBody.push(/=>\s*$/.test(head) || /\bfunction\b[^{}]*$/.test(head) || /\)\s*(:\s*[^={}()]*)?\s*$/.test(head));
      continue;
    }
    if (ch === '}') {
      depth--;
      fnBody.pop();
      continue;
    }
    if (ch === '(' || ch === '[') depth++;
    else if (ch === ')' || ch === ']') depth--;
    if (fnBody.includes(true)) continue; // inside a lazy body
    if (ch === '=' && masked[i + 1] === '>') {
      let j = i + 2;
      while (j < masked.length && /\s/.test(masked[j])) j++;
      if (masked[j] !== '{') skip = depth; // a braced body is handled by fnBody instead
      continue;
    }
    if (ch === 'n' && (i === 0 || !/[$\w]/.test(masked[i - 1]))) {
      const m = /^new\s+([$_A-Za-z][$\w$]*)/.exec(masked.slice(i));
      // bare `new X` without parens; member forms like `new Foo.Bar` capture `Foo` (conservative)
      if (m && !PURE_CONSTRUCTORS.has(m[1])) return true;
    }
    if (ch !== '(') continue;
    let j = i - 1;
    while (j >= 0 && masked[j] === ' ') j--;
    if (j < 0 || !/[$\w.\])]/.test(masked[j])) continue; // not preceded by a callee
    const tail = masked.slice(0, i).trimEnd();
    const word = /[$A-Za-z_$][$A-Za-z0-9_$]*$/.exec(tail);
    const name = word ? word[0] : '';
    if (name === 'function' || name === 'async') continue; // a declaration/arrow head, not a call
    const before = tail.slice(0, tail.length - name.length).trimEnd();
    if (/new$/.test(before)) {
      if (!PURE_CONSTRUCTORS.has(name)) return true; // `new` outside the allow-list
    } else {
      const close = matching(masked, i, '(', ')');
      // a method/getter head `name(...)` or `name(...): Type` declares a function: skip its whole body
      let body = -1;
      if (close !== -1) {
        let k = close + 1;
        while (masked[k] === ' ') k++;
        if (masked[k] === ':') { // skip a return-type annotation free of braces and parens
          let m = k + 1;
          while (m < masked.length && masked[m] !== '{' && masked[m] !== '}' && masked[m] !== '(' && masked[m] !== ')') m++;
          while (masked[m] === ' ') m++;
          if (masked[m] === '{') body = matching(masked, m, '{', '}');
        } else if (masked[k] === '{') body = matching(masked, k, '{', '}');
      }
      if (body !== -1) {
        i = body;
        continue;
      }
      return true; // a plain or member call
    }
  }
  return false;
}

// Index of the bracket that closes the one at `at`, or -1.
function matching(text, at, open, close) {
  let depth = 0;
  for (let j = at; j < text.length; j++) {
    if (text[j] === open) depth++;
    else if (text[j] === close) {
      depth--;
      if (depth === 0) return j;
    }
  }
  return -1;
}

// Allow-list over the stripped source: imports, type/class/function declarations, and pure
// bindings only. Anything else (if/for/try, expression statements, assignments, calls, `new`
// outside PURE_CONSTRUCTORS, IIFEs) is a module-level side effect.
function sideEffectViolations(sourceFile, text) {
  const stripped = stripTypeScriptTypes(text, { mode: 'strip' });
  const masked = mask(stripped);
  const lines = stripped.split('\n');
  const violations = [];
  for (const { text: s, line, start, end } of topLevelStatements(masked)) {
    let ok = false;
    if (/^import\b/.test(s)) {
      if (/^import\s*\(/.test(s)) ok = false; // dynamic import is an expression that loads a module
      else if (/^import\s*['"]/.test(s)) {
        // side-effect-only import: allowed only into src/core/ or types/, which the graph rules prove pure
        const spec = /^\s*import\s*['"]([^'"]+)['"]/.exec(stripped.slice(start, end).trim())?.[1] ?? '';
        const target = resolve(dirname(sourceFile), spec);
        const file = spec === '' || /\.ts$/.test(target) ? target : `${target}.ts`;
        ok = spec !== '' && (under(file, 'src/core') || under(file, 'types'));
      } else ok = true;
    }
    else if (/^export\b/.test(s)) {
      if (/^export\s+type\b/.test(s)) ok = true;
      else if (/^export\s*\*/.test(s)) ok = true; // export ... from
      else if (/^export\s*\{/.test(s)) ok = true; // re-export
      else if (/^export\s+(?:async\s+function|function|class)\b/.test(s)) ok = true;
      else if (/^export\s+(?:const|let|var)\b/.test(s)) ok = initializerIsPure(stripped.slice(start, end).trim()); // raw: mask once inside
    } else if (/^(?:async\s+function|function|class)\b/.test(s)) ok = true;
    else if (/^(?:const|let|var)\b/.test(s)) ok = initializerIsPure(stripped.slice(start, end).trim()); // raw: mask once inside
    if (!ok) violations.push(`${rel(sourceFile)}: line has a module-level side effect: ${(lines[line - 1] || '').trim()}`);
  }
  return violations;
}

test('no source file under src has a module level side effect', () => {
  const dir = resolve(pkgDir, 'src');
  const files = readdirSync(dir, { recursive: true }).map(String).filter(name => name.endsWith('.ts'));
  assert.ok(files.length > 0);
  const violations = files.flatMap(name => sideEffectViolations(resolve(dir, name).replaceAll('\\', '/'), readFileSync(resolve(dir, name), 'utf8')));
  assert.deepEqual(violations, [], 'module-level side effects:\n' + violations.join('\n'));
});

// The resolve hook reports every resolved URL, so the child proves only type entries were loaded.
const LOADER_SOURCE = [
  "export async function resolve(specifier, context, next) {",
  "  const result = await next(specifier, context);",
  "  console.error('LOADED ' + result.url);",
  "  return result;",
  "}",
].join('\n');

test('importing a type entry evaluates no module of the package and exports no values', { timeout: 120000 }, () => {
  const childScript = `
    import { register } from 'node:module';
    register(process.env.ALEF_LOADER);
    const m = await import(process.env.ALEF_ENTRY);
    console.log(JSON.stringify(Object.keys(m)));
  `;
  const pkgFileUrl = pathToFileURL(pkgDir).href;
  for (const [entryFile, codeFile] of typeEntries) {
    const entryUrl = pathToFileURL(entryFile).href;
    const subpath = Object.keys(sources).find(sp => resolve(pkgDir, sources[sp]).replaceAll('\\', '/') === entryFile);
    const run = spawnSync(process.execPath, ['--experimental-strip-types', '--input-type=module', '-e', childScript], {
      timeout: 20000,
      encoding: 'utf8',
      env: {
        ...process.env,
        ALEF_ENTRY: entryUrl,
        ALEF_LOADER: `data:text/javascript,${encodeURIComponent(LOADER_SOURCE)}`,
      },
    });
    assert.equal(run.status, 0, `type entry "${subpath}" failed to import: ${run.stderr}`);
    const loaded = run.stderr.split('\n').filter(line => line.startsWith('LOADED ')).map(line => line.slice('LOADED '.length).trim());
    assert.ok(loaded.includes(entryUrl), `type entry "${subpath}" was not resolved by the hook: ${run.stderr}`);
    assert.deepEqual(JSON.parse(run.stdout.trim()), [], `type entry "${subpath}" exported runtime values`);
    for (const url of loaded) {
      if (url.startsWith(pkgFileUrl)) {
        assert.ok(url.startsWith(`${pkgFileUrl}/type-entries/`), `type entry "${subpath}" pulled in ${url}`);
      }
    }
    const codeUrl = pathToFileURL(codeFile).href;
    assert.ok(codeUrl.startsWith(pkgFileUrl), `code entry of "${subpath}" is outside the package`);
  }
});

test('a type entry is a clean leaf of the import graph and imports no foreign module', () => {
  const fsTypeEntry = resolve(pkgDir, sources['./fs/types']).replaceAll('\\', '/');
  assert.ok(fsTypeEntry);
  assert.deepEqual(checkEntry(resolve(pkgDir, sources['./fs/types'])), []);
  assert.deepEqual(checkEntry(resolve(pkgDir, sources['./menu/types'])), []);
  // menu carries the type-only edge to window; the walk stays clean through menu.ts and window.ts
});

test('a type entry must not import a foreign module outside its own code entry', () => {
  const fsTypeEntry = resolve(pkgDir, sources['./fs/types']).replaceAll('\\', '/');
  assert.equal(isAllowed(fsTypeEntry, resolve(pkgDir, 'src/desktop/window.ts').replaceAll('\\', '/')), false);
  assert.equal(isAllowed(fsTypeEntry, resolve(pkgDir, 'src/net/http.ts').replaceAll('\\', '/')), false);
  // the typeEntries branch itself must allow the edge to the code entry of the same subpath
  assert.equal(isAllowed(fsTypeEntry, typeEntries.get(fsTypeEntry)), true);
});

test('the side effect scanner flags module level statements that are not on the allow-list', () => {
  const file = `${pkgUrl}/src/x.ts`;
  const caught = [
    "globalThis.console.error('x');",
    '(() => {})();',
    'new TextEncoder();',
    'done = true;',
    "await fetch('x');",
    // a call nested in the condition of a top-level `if`
    'if (ready()) {\n  globalThis.console.error(\'x\');\n}',
    // a bare top-level block is a statement of its own
    '{ side(); }',
    "Object.defineProperty(globalThis, 'probe', { value: true });",
    // after stripping, `interface A {}` is gone and the assignment is a statement of its own
    'let x = 1\nglobalThis.__probe = true',
    // eager code after an arrow in the same initializer, braced and unbraced
    "const tools = { later: () => {}, eager: globalThis.console.error('probe') };",
    "const a = () => {}, b = fetch('/x');",
    // bare `new X` without parentheses is still a construction
    'const resource = new ExternalResource;',
    // a string terminal token ends the statement: the probe after it splits off
    "const s = 'a'\nglobalThis.__probe = true",
    // dynamic and side-effect-only imports load a module
    "import('./x.ts');",
    "import './x.ts';",
  ];
  for (const snippet of caught) {
    assert.equal(sideEffectViolations(file, snippet).length, 1, `not caught: ${snippet}`);
  }
  // the ASI-split second statement reports its own head line
  const v = sideEffectViolations(file, 'let x = 1\nglobalThis.__probe = true');
  assert.equal(v.length, 1);
  assert.match(v[0], /globalThis\.__probe/);
});

test('the side effect scanner accepts imports, declarations and pure bindings', () => {
  const file = `${pkgUrl}/src/x.ts`;
  const clean = [
    'const x = 1;',
    'export const y = () => { run(); };',
    "import type { A } from './a.ts';",
    'type B = number;',
    'interface C {}',
    'export class D {}',
    'function e() {}',
    "const enc = new TextEncoder();",
    'let p;',
    // the continuation of a multi-line binding starts with `&&`
    'const ok = a > 0\n  && b > 0;',
    // an unbraced arrow makes its own body lazy, but eager code after it is still caught
    'const q = (input: string) => new URLSearchParams(input);',
    "import x from './x.ts';",
    "import { a } from './x.ts';",
  ];
  for (const snippet of clean) {
    assert.deepEqual(sideEffectViolations(file, snippet), [], `false positive: ${snippet}`);
  }
});

test('the type statement scanner splits same-line statements and rejects non type code', () => {
  assert.equal(typeStatementTextViolations('t', "export type * from './x.ts'; globalThis.__probe = true;").length, 1);
  // the same probe across a newline without a semicolon still splits and is caught
  assert.equal(typeStatementTextViolations('t', "export type * from './x.ts'\nglobalThis.__probe = true").length, 1);
  assert.equal(typeStatementTextViolations('t', 'interface A {} globalThis.__probe = true;').length, 1);
  // a re-export specifier without the `type` keyword is a value export
  assert.equal(typeStatementTextViolations('t', "export { type A, B } from './x.ts';").length, 1);
  assert.equal(typeStatementTextViolations('t', 'export const x = 1;').length, 1);
  // a line ending in a string ends the statement: the assignment splits off and is caught
  assert.equal(typeStatementTextViolations('t', "type A = 'literal'\nglobalThis.__probe = true;").length, 1);
  const [msg] = typeStatementTextViolations('t', "type A = 'literal'\nglobalThis.__probe = true;");
  assert.match(msg, /globalThis\.__probe = true/);
});

test('the type statement scanner accepts type statements and generated re-exports', () => {
  assert.deepEqual(typeStatementTextViolations('t', "export type * from './x.ts';"), []);
  assert.deepEqual(typeStatementTextViolations('t', "import type { A } from './b.ts';\nexport type * from './a.ts';"), []);
  assert.deepEqual(typeStatementTextViolations('t', "export { type A, type B } from './x.ts';"), []);
  assert.deepEqual(typeStatementTextViolations('t', 'interface A {\n  b: string;\n}\ntype C = number;'), []);
  // a union of object types is one statement (ts-rs emits tagged enums this way)
  assert.deepEqual(typeStatementTextViolations('t', 'export type U = { "k": "a", n: string, } | { "k": "b", };'), []);
  assert.equal(typeStatementTextViolations('t', 'export type U = { a: string } | { b: string }\nglobalThis.__probe = true;').length, 1);
  // `export * from` is allowed only in the generated types directory
  assert.equal(typeStatementTextViolations('t', "export * from './x.ts';").length, 1);
  assert.deepEqual(typeStatementTextViolations('t', "export * from './x.ts';", true), []);
});

// The publishable build (API-4): dist/ mirrors the exports map of the package, whose every condition target it must ship.
let dist;
function buildDist() {
  if (dist === undefined) {
    const run = spawnSync(process.execPath, [resolve(pkgDir, 'build.mjs')], { timeout: 240000, encoding: 'utf8' });
    assert.equal(run.status, 0, `the build of @alef-runtime/api failed:\n${run.stdout}\n${run.stderr}`);
    dist = resolve(pkgDir, 'dist');
  }
  return dist;
}

test('the_build_starts_from_a_clean_dist', () => {
  const stale = resolve(pkgDir, 'dist', 'stale');
  mkdirSync(stale, { recursive: true });
  writeFileSync(resolve(stale, 'junk.js'), 'export const stale = true;\n');
  buildDist();
  assert.ok(!existsSync(stale), 'files of an earlier build must not survive into the tarball');
});

test('after_the_build_every_exported_subpath_has_its_dist_files', () => {
  buildDist();
  for (const [subpath, value] of Object.entries(exportsMap)) {
    assert.ok(existsSync(resolve(pkgDir, value['types'])), `subpath "${subpath}" has no ${value['types']}`);
    if (subpath === './types' || subpath.endsWith('/types')) {
      assert.equal(value['import'], undefined, `type-only subpath "${subpath}" must not be importable at runtime`);
      assert.equal(value['default'], undefined, `type-only subpath "${subpath}" must not be importable at runtime`);
    } else {
      assert.ok(existsSync(resolve(pkgDir, value['import'])), `subpath "${subpath}" has no ${value['import']}`);
      assert.equal(value['default'], value['import'], `subpath "${subpath}" must resolve "default" to the same module as "import"`);
    }
  }
});

test('type_only_subpaths_leave_no_js_stub_in_dist', () => {
  const out = buildDist();
  const stubs = readdirSync(out, { recursive: true }).map(String).map(name => name.replaceAll('\\', '/'))
    .filter(name => name.endsWith('.js') && (name.startsWith('types/') || name.startsWith('type-entries/')));
  assert.deepEqual(stubs, [], 'type-only entries must not ship a runtime stub');
});

test('the_built_modules_load_in_node_with_rewritten_specifiers', async () => {
  buildDist();
  const root = await import(pathToFileURL(resolve(pkgDir, 'dist/src/index.js')).href);
  assert.equal(typeof root.fs.readDir, 'function');
  const core = await import(pathToFileURL(resolve(pkgDir, 'dist/src/core/index.js')).href);
  assert.equal(typeof core.call, 'function');
  assert.equal(core.AlefError.name, 'AlefError');
});

test('no_declaration_in_dist_names_a_ts_source', () => {
  const out = buildDist();
  const files = readdirSync(out, { recursive: true }).map(String).filter(name => name.endsWith('.d.ts'));
  assert.ok(files.length >= 48, `expected a declaration per subpath, got ${files.length}`);
  const offenders = files.filter(name => /from\s*['"]\.\.?\/[^'"]*\.ts['"]/.test(readFileSync(resolve(out, name), 'utf8')));
  assert.deepEqual(offenders, [], `declarations still name .ts sources: ${offenders.join(', ')}`);
});

test('the_declaration_cleanup_rewrites_relative_ts_specifiers_and_nothing_else', () => {
  assert.equal(rewriteDeclarationSpecifiers("export type * from '../../src/data/fs.ts';"), "export type * from '../../src/data/fs.js';");
  assert.equal(rewriteDeclarationSpecifiers("import type { A } from './b.ts';\nexport type { C } from './d.js';"), "import type { A } from './b.js';\nexport type { C } from './d.js';");
  assert.equal(rewriteDeclarationSpecifiers("export { type A } from '@alef-runtime/api';"), "export { type A } from '@alef-runtime/api';");
  assert.equal(rewriteDeclarationSpecifiers("type B = import('./x.ts').C;"), "type B = import('./x.js').C;");
});

test('npm_pack_ships_every_advertised_file_and_nothing_else', { timeout: 120000 }, () => {
  buildDist();
  const run = spawnSync('npm', ['pack', '--dry-run', '--json'], {
    cwd: pkgDir, timeout: 90000, encoding: 'utf8', shell: process.platform === 'win32',
  });
  assert.equal(run.status, 0, `npm pack failed: ${run.stderr}`);
  // newer npm reports the dry-run paths without the "package/" prefix: normalize
  const listed = new Set(JSON.parse(run.stdout)[0].files.map(file => file.path.replace(/^package\//, '')));
  assert.ok(listed.has('dist/src/index.js'), `no dist in the tarball: ${[...listed].join(', ')}`);
  for (const [subpath, value] of Object.entries(exportsMap)) {
    for (const target of [value['types'], value['import'], value['default']]) {
      if (typeof target !== 'string') continue;
      assert.ok(listed.has(target.slice(2)), `the tarball has no ${target} advertised by "${subpath}"`);
    }
    assert.ok(!listed.has(sources[subpath].slice(2)), `the tarball must not ship the unadvertised source ${sources[subpath]}`);
  }
  const strangers = [...listed].filter(entry => entry !== 'package.json' && !entry.startsWith('dist/'));
  assert.deepEqual(strangers, [], `the tarball ships files outside dist: ${strangers.join(', ')}`);
});
