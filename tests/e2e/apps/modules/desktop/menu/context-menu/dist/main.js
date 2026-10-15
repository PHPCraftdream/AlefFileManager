// SPDX-License-Identifier: MIT OR Apache-2.0
import { suite, verdict } from './harness.js';
import { fs } from './src/data/fs.js';
import { shell } from './src/desktop/shell.js';
const { note, platform, mode } = JSON.parse(await (await fetch('./targets.json')).text());
const checks = suite();
const flatten = items => items.flatMap(item => [item, ...(item.submenu ? flatten(item.submenu) : [])]);
const codeOf = async promise => {
  try { await promise; return ''; }
  catch (error) { return String(error.code ?? error.name ?? '').toUpperCase().replaceAll('-', '_'); }
};
const permissionDenied = async promise => {
  const code = await codeOf(promise);
  if (!code.includes('PERMISSION_DENIED')) throw new Error(`expected permission denied, got ${code || 'success'}`);
};
if (mode === 'allowed') {
  await checks.check('context-menu-allowed-lists-and-safely-refuses', async () => {
    const menu = await shell.contextMenu([note]);
    try {
      if (!menu.items.length) throw new Error('real text file menu is empty');
      const all = flatten(menu.items);
      if (platform === 'win32') {
        if (!all.some(item => item.enabled && !item.submenu && item.id.startsWith('cmd:'))) throw new Error('no enabled native Windows command');
      } else if (!all.some(item => item.id === 'open') || !all.some(item => item.id === 'reveal')) throw new Error('fallback menu lacks open/reveal');
      const defaultLeaf = all.find(item => item.default && item.enabled && !item.submenu && !item.separator);
      if (!defaultLeaf) throw new Error('text file menu has no default enabled actionable leaf');
      if (await fs.readText(note) !== 'context menu note\n') throw new Error('scoped note was not readable');
      await permissionDenied(menu.invoke('runas'));
    } finally { await menu.close(); }
  });
} else if (mode === 'substituted') {
  await checks.check('context-menu-substitute-lists-no-items-and-refuses-invoke', async () => {
    const menu = await shell.contextMenu([note]);
    try {
      if (menu.items.length) throw new Error('substituted menu is not empty');
      await permissionDenied(menu.invoke('runas'));
    } finally { await menu.close(); }
  });
} else {
  await checks.check('context-menu-undeclared-right-is-refused', async () => {
    await permissionDenied(shell.contextMenu([note]));
  });
}
await verdict(checks.failed());
