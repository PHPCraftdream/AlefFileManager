// SPDX-License-Identifier: MIT OR Apache-2.0
// Real OS registrations only. No synthetic pressed events or keyboard injection.
import { api, guard, rejection, report, suite, until, verdict } from './harness.js';

const KEY = 'Ctrl+Alt+Shift+F20';
const options = () => ({ signal: AbortSignal.timeout(10000) });
const expectCode = async (promise, code) => {
  const error = await rejection(promise);
  if (error?.code !== code) throw new Error(`expected ${code}, got ${error?.code ?? 'success'}`);
};

async function main() {
  const { check, failed } = suite();
  const self = await api.window.current();
  const query = new URL(location.href).searchParams;
  if (query.has('foreign')) {
    let registration;
    try {
      await check('shortcut-foreign-document-handle-is-denied', () =>
        expectCode(api.call('shortcut.unregister', { id: query.get('foreign') }, options()), 'NOT_FOUND'));
      await self.setTitle('foreign-ok');
      await until(async () => (await self.state()).title === 'register-now',
        20000, 'main to release its native reservation');
      registration = await api.call('shortcut.register', { accelerator: KEY }, options());
      await self.setTitle(JSON.stringify({ registration, failed: failed() }));
      // Main closes this window while it owns the key: session teardown is the cleanup.
      // If startup fails instead, explicitly roll back here.
    } catch (error) {
      if (registration) await api.call('shortcut.unregister', { id: registration.id }, options());
      throw error;
    }
    return;
  }

  const { mode } = await (await fetch('targets.json')).json();
  const registrations = new Set();
  let child;
  const register = async (accelerator = KEY) => {
    const handle = await api.shortcut.register(accelerator, options());
    registrations.add(handle);
    return handle;
  };
  const unregister = async handle => {
    await handle.unregister(options());
    registrations.delete(handle);
  };
  try {
    if (mode === 'denied') {
      await check('shortcut-denied-registration', () =>
        expectCode(register(), 'PERMISSION_DENIED'));
    } else if (mode === 'substituted') {
      await check('shortcut-substituted-register-unregister', async () => {
        const reply = await api.call('shortcut.register', { accelerator: KEY }, options());
        const handle = new api.Shortcut(reply);
        registrations.add(handle);
        if (typeof reply.id !== 'string' || !Number.isInteger(reply.owner) || reply.token !== null) {
          throw new Error(`not an inert registration: ${JSON.stringify(reply)}`);
        }
        const off = await handle.on('pressed', () => { throw new Error('inert callback fired'); }, options());
        try { await unregister(handle); } finally { off(); }
        return 'token=null; no OS press claimed (no-host coverage is in fixture tests)';
      });
    } else {
      let first;
      await check('shortcut-native-register', async () => {
        first = await register();
        if (!/^s\d+:r\d+$/.test(first.id)) throw new Error(`bad resource handle ${first.id}`);
      });
      await check('shortcut-native-duplicate-is-already-exists', async () => {
        // Track even an unexpected success so finally still releases it.
        await expectCode(register(), 'ALREADY_EXISTS');
      });
      await check('shortcut-invalid-accelerator', () =>
        expectCode(register('Ctrl+Alt+DefinitelyNotAKey'), 'INVALID_ARGUMENT'));
      await check('shortcut-unregister-and-register-again', async () => {
        const old = first.id;
        await unregister(first);
        first = await register();
        if (first.id === old) throw new Error('resource handle reused');
      });
      await check('shortcut-child-close-releases-registration', async () => {
        const foreign = first.id;
        child = await api.window.create({ label: 'shortcut-child',
          url: `/index.html?foreign=${encodeURIComponent(foreign)}`, width: 320, height: 240 });
        await until(async () => (await child.state()).title === 'foreign-ok',
          20000, 'child to deny the live foreign handle');
        await expectCode(register(), 'ALREADY_EXISTS');
        await unregister(first);
        await child.setTitle('register-now');
        let reply;
        await until(async () => {
          const title = (await child.state()).title;
          if (!title.startsWith('{')) return false;
          reply = JSON.parse(title);
          return true;
        }, 20000, 'child registration and foreign-handle check');
        if (reply.failed.length) throw new Error(`child checks failed: ${reply.failed}`);
        await expectCode(api.call('shortcut.unregister', { id: reply.registration.id }, options()), 'NOT_FOUND');
        await expectCode(register(), 'ALREADY_EXISTS');
        await child.close();
        await until(async () => !(await api.window.all()).some(w => w.label === child.label),
          10000, 'child window close');
        child = undefined;
        // The UI sweep may release the reservation shortly after the window disappears.
        await until(async () => {
          try { first = await register(); return true; }
          catch (error) { if (error.code === 'ALREADY_EXISTS') return false; throw error; }
        }, 10000, 'closed child to release its native reservation');
      });
      await report('MANUAL shortcut-os-pressed-not-tested; requires OS input/SendInput check');
    }
  } finally {
    // Child registration is deliberately abandoned to real session teardown, never faked.
    await check('shortcut-cleanup', async () => {
      const errors = [];
      if (child) {
        try {
          await child.destroy();
          await until(async () => !(await api.window.all()).some(w => w.label === child.label),
            10000, 'child cleanup');
        } catch (error) { errors.push(error.message); }
      }
      for (const handle of registrations) {
        try { await handle.unregister(options()); } catch (error) { errors.push(error.message); }
      }
      if (errors.length) throw new Error(errors.join('; '));
    });
  }
  await verdict(failed());
}

guard(main);
