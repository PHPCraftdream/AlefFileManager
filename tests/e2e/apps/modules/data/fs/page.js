// fs scenario: files and folders through the real transport. In `real` mode the user allowed the scope;
// in `substituted` mode he chose a stand-in: the page finds an empty folder, writes into it and
// finds its own writes again, and the runner looks at the real folder afterwards.
import { api, guard, rejection, suite, verdict } from './harness.js';

const { fs } = api;

const sep = path => (path.includes('\\') ? '\\' : '/');
const join = (base, ...names) => [base.replace(/[\\/]+$/, ''), ...names].join(sep(base));

const expectCode = async (what, promise, code) => {
  const error = await rejection(promise);
  if (error?.code !== code) throw new Error(`${what}: ${error?.code ?? 'it succeeded'}, expected ${code}`);
  return error;
};

async function real(t, check) {
  const dir = join(t.root, 'work');

  await check('fs-text-and-bytes-round-trip-with-unicode', async () => {
    await fs.mkdir(dir);
    const file = join(dir, 'note.txt');
    await fs.writeText(file, 'héllo — мир 🌍');
    if (await fs.readText(file) !== 'héllo — мир 🌍') throw new Error('the text did not come back');
    await fs.writeText(file, '!', { append: true });
    if (await fs.readText(file) !== 'héllo — мир 🌍!') throw new Error('append');
    const bytes = new Uint8Array(70000).map((_, index) => (index * 31) & 255);
    await fs.writeBytes(join(dir, 'a.bin'), bytes);
    const back = await fs.readBytes(join(dir, 'a.bin'));
    if (back.length !== bytes.length || back.some((byte, index) => byte !== bytes[index])) throw new Error('the bytes did not come back');
    await expectCode('create: false', fs.writeText(join(dir, 'nope.txt'), 'x', { create: false }), 'NOT_FOUND');
    if (await fs.exists(join(dir, 'nope.txt'))) throw new Error('create: false made the file');
  });

  await check('fs-stat-readdir-rename-copy-and-remove', async () => {
    const stat = await fs.stat(join(dir, 'note.txt'));
    if (stat.kind !== 'file' || stat.size !== new TextEncoder().encode('héllo — мир 🌍!').length) throw new Error(JSON.stringify(stat));
    if ((await fs.stat(dir)).kind !== 'dir') throw new Error('a folder is a dir');
    await fs.mkdir(join(dir, 'x', 'y'), { recursive: true });
    await fs.rename(join(dir, 'a.bin'), join(dir, 'x', 'b.bin'));
    await fs.copy(join(dir, 'note.txt'), join(dir, 'x', 'y', 'copy.txt'));
    await fs.copy(join(dir, 'x'), join(dir, 'x2'));
    const names = (await fs.readDir(dir)).map(entry => `${entry.name}:${entry.kind}`);
    if (JSON.stringify(names) !== JSON.stringify(['note.txt:file', 'x:dir', 'x2:dir'])) throw new Error(JSON.stringify(names));
    const entry = (await fs.readDir(dir))[0];
    if (entry.path !== join(dir, 'note.txt')) throw new Error(`the path of an entry: ${entry.path}`);
    await expectCode('a folder that is not empty', fs.remove(join(dir, 'x')), 'DIRECTORY_NOT_EMPTY');
    await fs.remove(join(dir, 'x'), { recursive: true });
    await expectCode('a missing file', fs.readText(join(dir, 'x', 'b.bin')), 'NOT_FOUND');
    await expectCode('a file as a folder', fs.readDir(join(dir, 'note.txt')), 'NOT_A_DIRECTORY');
    await expectCode('a folder as a file', fs.readText(dir), 'IS_A_DIRECTORY');
    await expectCode('a folder made twice', fs.mkdir(dir), 'ALREADY_EXISTS');
  });

  await check('fs-outside-the-scope-is-denied-however-the-path-is-spelled', async () => {
    const sneaking = `${dir}${sep(dir)}..${sep(dir)}..${sep(dir)}${t.outsideName}${sep(dir)}secret.txt`;
    for (const path of [t.outside, sneaking, 'relative.txt', '']) {
      await expectCode(`readText ${path}`, fs.readText(path), 'PERMISSION_DENIED');
      await expectCode(`stat ${path}`, fs.stat(path), 'PERMISSION_DENIED');
      await expectCode(`writeText ${path}`, fs.writeText(path, 'x'), 'PERMISSION_DENIED');
      await expectCode(`remove ${path}`, fs.remove(path), 'PERMISSION_DENIED');
    }
    await expectCode('copy out', fs.copy(join(dir, 'note.txt'), t.outside), 'PERMISSION_DENIED');
    await expectCode('rename out', fs.rename(join(dir, 'note.txt'), t.outside), 'PERMISSION_DENIED');
  });

  await check('fs-a-link-that-leads-out-of-the-scope-leads-nowhere', async () => {
    if (!t.link) return 'skipped: the runner could not make a link';
    await expectCode('through the link', fs.readText(t.link), 'PERMISSION_DENIED');
    const entry = await fs.lstat(t.link);
    if (entry.kind !== 'symlink') throw new Error(`lstat: ${entry.kind}`);
    const listed = (await fs.readDir(t.root)).find(item => item.name === 'link');
    if (listed?.kind !== 'symlink') throw new Error(`readDir shows ${JSON.stringify(listed)}`);
    await fs.remove(t.link);
    if (await rejection(fs.lstat(t.link)) === null) throw new Error('the link is still there');
    return undefined;
  });

  await check('fs-a-scratch-file-and-a-picked-file-need-no-scope', async () => {
    const scratch = await fs.tempFile();
    await fs.writeText(scratch, 'scratch');
    if (await fs.readText(scratch) !== 'scratch') throw new Error('the scratch file');
    const folder = await fs.tempDir();
    await fs.writeText(join(folder, 'in.txt'), 'in');
    if ((await fs.readDir(folder)).length !== 1) throw new Error('the scratch folder');
    const [picked] = await api.dialog.open();
    if (!picked) throw new Error('the scripted dialog gave nothing');
    if (await fs.readText(picked) !== 'picked') throw new Error('the picked file');
    await expectCode('what is next to the picked file', fs.readText(join(picked, '..', 'neighbour.txt')), 'PERMISSION_DENIED');
    await expectCode('writing a file that was picked for reading', fs.writeText(picked, 'x'), 'PERMISSION_DENIED');
  });

  await check('fs-a-whole-file-is-for-small-files', async () => {
    const error = await expectCode('a big file whole', fs.readBytes(t.big), 'INVALID_ARGUMENT');
    if (!/fs\.open/.test(error.message)) throw new Error(`the hint: ${error.message}`);
    await expectCode('writing a big file whole', fs.writeBytes(join(dir, 'big.out'), new Uint8Array(64 * 1024 * 1024 + 1)), 'INVALID_ARGUMENT');
  });
}

async function substituted(t, check) {
  await check('fs-a-stand-in-starts-as-an-empty-folder', async () => {
    if ((await fs.stat(t.root)).kind !== 'dir') throw new Error('the scope is a folder');
    const listed = await fs.readDir(t.root);
    if (listed.length !== 0) throw new Error(`the folder is not empty: ${JSON.stringify(listed)}`);
    await expectCode('what is really there', fs.readText(join(t.root, 'real.txt')), 'NOT_FOUND');
    if (await fs.exists(join(t.root, 'real.txt'))) throw new Error('it exists');
  });

  await check('fs-a-stand-in-keeps-what-is-written-and-answers-like-a-disk', async () => {
    const note = join(t.root, 'note.txt');
    await fs.writeText(note, 'mine');
    if (await fs.readText(note) !== 'mine') throw new Error('the write was lost');
    await fs.mkdir(join(t.root, 'a', 'b'), { recursive: true });
    await fs.writeText(join(t.root, 'a', 'b', 'deep.txt'), 'deep');
    await fs.rename(note, join(t.root, 'renamed.txt'));
    await fs.copy(join(t.root, 'renamed.txt'), join(t.root, 'copied.txt'));
    const names = (await fs.readDir(t.root)).map(entry => entry.name);
    if (JSON.stringify(names) !== JSON.stringify(['a', 'copied.txt', 'renamed.txt'])) throw new Error(JSON.stringify(names));
    await expectCode('a folder that is not empty', fs.remove(join(t.root, 'a')), 'DIRECTORY_NOT_EMPTY');
    await expectCode('a missing file', fs.readText(join(t.root, 'gone.txt')), 'NOT_FOUND');
    const entry = (await fs.readDir(t.root))[0];
    if (!entry.path.startsWith(t.root.replace(/[\\/]+$/, ''))) throw new Error(`a path that is not the application's own: ${entry.path}`);
  });

  await check('fs-a-stand-in-refuses-what-the-scope-refuses', async () => {
    await expectCode('outside', fs.readText(t.outside), 'PERMISSION_DENIED');
    await expectCode('writing outside', fs.writeText(t.outside, 'x'), 'PERMISSION_DENIED');
  });
}

async function main() {
  const t = await (await fetch('targets.json')).json();
  const { check, failed } = suite();
  if (t.mode === 'substituted') await substituted(t, check);
  else await real(t, check);
  await verdict(failed());
  await api.app.quit(0);
}

guard(main);
