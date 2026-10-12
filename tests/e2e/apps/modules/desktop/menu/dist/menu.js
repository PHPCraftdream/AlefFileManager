// SPDX-License-Identifier: MIT OR Apache-2.0
// Real muda attachment only; no synthetic clicks or keyboard injection, and no popup that would wait for a person.
import { api, guard, rejection, report, suite, verdict } from './harness.js';

const options = () => ({ signal: AbortSignal.timeout(10000) });
const expectCode = async (promise, code) => {
  const error = await rejection(promise);
  if (error?.code !== code) throw new Error(`expected ${code}, got ${error?.code ?? 'success'}`);
};
const root = items => [{ kind: 'submenu', id: 'file', label: 'File', items }];
const tree = label => root([
  { id: 'action', label, accelerator: 'Ctrl+Alt+Shift+F20' },
  { kind: 'check', id: 'checked', label: 'Checked', checked: true },
  { kind: 'separator' },
  { kind: 'submenu', id: 'nested', label: 'Nested', items: [
    { id: 'disabled', label: 'Disabled', enabled: false },
  ] },
]);

async function main() {
  const { check, failed } = suite();
  const { platform } = await (await fetch('targets.json', options())).json();
  const self = await api.window.current(options());
  const set = items => api.menu.setApplicationMenu(items, options());
  const windowSet = items => api.menu.setWindowMenu(self, items, options());
  const windowCode = platform === 'darwin' || platform === 'linux' ? 'NOT_AVAILABLE' : null;
  try {
    await check('menu-malformed-trees-are-rejected', async () => {
      for (const items of [
        [{ id: 'missing-label' }],
        [{ kind: 'submenu', id: 'missing-items', label: 'Bad' }],
        root([{ id: 'unknown-field', label: 'Bad', typo: true }]),
        root([{ id: 'normal', label: 'Bad', checked: true }]),
      ]) await expectCode(set(items), 'INVALID_ARGUMENT');
    });
    await check('menu-duplicate-ids-across-the-tree-are-rejected', () =>
      expectCode(set(root([{ id: 'file', label: 'Duplicate' }])), 'INVALID_ARGUMENT'));
    await check('menu-invalid-accelerators-are-rejected', () =>
      expectCode(set(root([{ id: 'bad-key', label: 'Bad', accelerator: 'Ctrl+DefinitelyNotAKey' }])), 'INVALID_ARGUMENT'));
    await check('menu-roles-with-custom-ids-are-rejected', () =>
      expectCode(set(root([{ role: 'copy', id: 'bad-role' }])), 'INVALID_ARGUMENT'));
    await check('menu-invalid-popup-coordinates-are-rejected', async () => {
      for (const coordinates of [{ x: 1000001, y: 0 }, { x: 0, y: -1000001 }, { x: 'bad', y: 0 }, { x: 1 }, { y: 1 }]) {
        await expectCode(api.menu.popup([], { ...coordinates, ...options() }), 'INVALID_ARGUMENT');
      }
    });
    await check('menu-invalid-popup-items-are-rejected-before-it-is-shown', () =>
      expectCode(api.menu.popup([{ id: 'twice', label: 'One' }, { id: 'twice', label: 'Two' }], options()), 'INVALID_ARGUMENT'));
    if (platform === 'linux') {
      await check('menu-linux-native-operations-are-not-available', async () => {
        await expectCode(set(tree('Unavailable')), 'NOT_AVAILABLE');
        await expectCode(windowSet(tree('Unavailable')), 'NOT_AVAILABLE');
        await expectCode(api.menu.popup([], options()), 'NOT_AVAILABLE');
        return 'GTK is not owned by winit; no native menu acceptance claimed';
      });
    } else {
      await check('menu-native-nested-items-and-accelerator-are-accepted', () => set(tree('First')));
      await check('menu-application-or-caller-window-menu-can-be-replaced', () => set(tree('Replacement')));
      await check('menu-explicit-own-window-menu-follows-platform-support', async () => {
        if (windowCode) await expectCode(windowSet(tree('Own window')), windowCode);
        else await windowSet(tree('Own window'));
        return windowCode ?? 'accepted on the caller window';
      });
      await check('menu-missing-target-window-is-not-found', () =>
        expectCode(api.call('menu.setWindowMenu', { label: 'menu-never-created', items: tree('Missing') }, options()), 'NOT_FOUND'));
      await check('menu-empty-popup-returns-null', async () => {
        const reply = await api.menu.popup([], options());
        if (reply !== null) throw new Error(`expected null, got ${JSON.stringify(reply)}`);
      });
      await check('menu-edit-and-minimize-roles-are-accepted', () =>
        set(root(['copy', 'cut', 'paste', 'selectAll', 'undo', 'redo', 'minimize'].map(role => ({ role })))));
      await check('menu-quit-and-about-roles-are-not-available', async () => {
        for (const role of ['quit', 'about']) await expectCode(set(root([{ role }])), 'NOT_AVAILABLE');
      });
      if (platform === 'darwin') {
        await check('menu-macos-application-roots-must-be-submenus', () =>
          expectCode(set([{ id: 'root-action', label: 'Unsupported root' }]), 'NOT_AVAILABLE'));
        await check('menu-macos-popup-is-not-available', () =>
          expectCode(api.menu.popup([{ id: 'popup', label: 'Not shown' }], options()), 'NOT_AVAILABLE'));
      }
      await check('menu-application-menu-can-be-cleared', () => set([]));
    }
    await report('MANUAL menu-click-accelerator-role-and-popup-not-tested');
  } finally {
    await check('menu-cleanup-clears-menus-or-confirms-platform-unavailability', async () => {
      const errors = [];
      // Try both clears even if one fails; no child window or global hotkey is created.
      for (const [clear, code] of [[() => set([]), platform === 'linux' ? 'NOT_AVAILABLE' : null],
        [() => windowSet([]), windowCode]]) {
        try {
          if (code) await expectCode(clear(), code);
          else await clear();
        } catch (error) { errors.push(error.message); }
      }
      if (errors.length) throw new Error(errors.join('; '));
    });
  }
  await verdict(failed());
}

guard(main);
