// System modules scenario: `path` (well-known directories, lexical arithmetic) and `os` (facts of the
// machine, theme). The directories are also reported to the runner (`path <name> <value>`), which
// checks on the disk that the ones that must exist do.
import { api, guard, rejection, report, suite, verdict } from './harness.js';

const PLATFORMS = { win32: 'windows', darwin: 'macos', linux: 'linux' };
const ARCHES = { x64: 'x86_64', arm64: 'aarch64', ia32: 'x86' };
const ID = 'org.alef.e2e.modules.system';

async function main() {
  const { sep, platform, arch, hostname } = await (await fetch('targets.json')).json();
  const { check, failed } = suite();
  const join = (...parts) => parts.join(sep);
  const absolute = text => text.startsWith('/') || /^[A-Za-z]:[\\/]/.test(text) || text.startsWith('\\\\');

  await check('path-directories-are-absolute-and-the-app-ones-end-with-the-id', async () => {
    for (const name of ['appData', 'appConfig', 'appCache', 'temp', 'home', 'documents', 'downloads', 'desktop', 'executable']) {
      const value = await api.path[name]();
      if (typeof value !== 'string' || !absolute(value)) throw new Error(`${name}: ${JSON.stringify(value)}`);
      await report(`path ${name} ${value}`);
      if (name.startsWith('app') && value.split(/[\\/]/).at(-1) !== ID) throw new Error(`${name} does not end with the app id: ${value}`);
    }
  });
  await check('path-join-normalize-dirname-basename', async () => {
    const expect = async (what, actual, expected) => {
      if (actual !== expected) throw new Error(`${what}: ${JSON.stringify(actual)}, expected ${JSON.stringify(expected)}`);
    };
    await expect('join', await api.path.join('a', 'b', '..', 'c', 'd.txt'), join('a', 'c', 'd.txt'));
    await expect('join of nothing', await api.path.join(), '.');
    await expect('normalize', await api.path.normalize('x/./y//z/..'), join('x', 'y'));
    await expect('dirname', await api.path.dirname('x/y/z.txt'), join('x', 'y'));
    await expect('dirname of a name', await api.path.dirname('z.txt'), '.');
    await expect('basename', await api.path.basename('x/y/z.txt'), 'z.txt');
    await expect('basename of a directory', await api.path.basename('x/y/'), 'y');
  });
  await check('os-info-describes-this-machine', async () => {
    const info = await api.os.info();
    if (info.platform !== PLATFORMS[platform]) throw new Error(`platform ${info.platform}, expected ${PLATFORMS[platform]}`);
    if (info.arch !== ARCHES[arch]) throw new Error(`arch ${info.arch}, expected ${ARCHES[arch]}`);
    if (info.hostname.toLowerCase() !== hostname.toLowerCase()) throw new Error(`hostname ${info.hostname}, expected ${hostname}`);
    for (const key of ['version', 'locale']) {
      if (typeof info[key] !== 'string' || info[key].trim() === '') throw new Error(`${key} is empty`);
    }
    if (info.version === 'unknown') throw new Error('the OS version could not be read');
    await report(`os ${JSON.stringify(info)}`);
  });
  await check('os-theme-is-light-or-dark', async () => {
    const theme = await api.os.theme();
    if (theme !== 'light' && theme !== 'dark') throw new Error(`theme ${JSON.stringify(theme)}`);
    return theme;
  });
  await check('os-theme-changed-subscription-can-be-made-and-undone', async () => {
    const off = await api.os.on('theme-changed', () => {});
    off();
  });
  await check('screen-agrees-with-the-state-of-the-window', async () => {
    const monitors = await api.screen.monitors();
    const state = await (await api.window.current()).state();
    if (monitors.length === 0) throw new Error('no display');
    if (state.x === null || state.y === null) throw new Error('the window position is unknown');
    const centre = { x: state.x + state.width / 2, y: state.y + state.height / 2 };
    const on = monitors.some(({ bounds: b }) => centre.x >= b.x && centre.x < b.x + b.width && centre.y >= b.y && centre.y < b.y + b.height);
    if (!on) throw new Error(`the centre of the window ${JSON.stringify(centre)} is on no display: ${JSON.stringify(monitors.map(monitor => monitor.bounds))}`);
    const cursor = await rejection(api.screen.cursorPosition());
    if (cursor !== null && cursor.code !== 'NOT_AVAILABLE') throw new Error(`cursorPosition failed: ${cursor.message}`);
    const point = cursor === null ? await api.screen.cursorPosition() : null;
    if (point !== null && !(Number.isFinite(point.x) && Number.isFinite(point.y))) throw new Error(`cursor ${JSON.stringify(point)}`);
    return point === null ? 'cursor not available here' : `cursor ${point.x},${point.y}`;
  });
  await check('window-create-is-denied-without-the-permission', async () => {
    const error = await rejection(api.window.create({ label: 'extra', url: '/index.html', width: 100, height: 100 }));
    if (error?.code !== 'PERMISSION_DENIED') throw new Error(`window.create: ${error?.code ?? 'it succeeded'}`);
    const open = await api.window.all();
    if (open.length !== 1) throw new Error(`${open.length} windows are open`);
  });

  await verdict(failed());
}

guard(main);
