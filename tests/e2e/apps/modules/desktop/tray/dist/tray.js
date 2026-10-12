// SPDX-License-Identifier: MIT OR Apache-2.0
// Real tray-icon on Windows/macOS: the calls are made, nothing is clicked. The icon of the check lives for a
// moment and is destroyed in every outcome.
import { api, guard, rejection, report, suite, verdict } from './harness.js';

const options = () => ({ signal: AbortSignal.timeout(10000) });
const expectCode = async (promise, code) => {
  const error = await rejection(promise);
  if (error?.code !== code) throw new Error(`expected ${code}, got ${error?.code ?? 'success'}`);
};

const table = new Uint32Array(256).map((_, n) => {
  let c = n;
  for (let k = 0; k < 8; k += 1) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});
const crc32 = bytes => {
  let c = 0xffffffff;
  for (const byte of bytes) c = table[(c ^ byte) & 255] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
};
const adler32 = bytes => {
  let a = 1;
  let b = 0;
  for (const byte of bytes) {
    a = (a + byte) % 65521;
    b = (b + a) % 65521;
  }
  return ((b << 16) | a) >>> 0;
};
const be32 = n => [(n >>> 24) & 255, (n >>> 16) & 255, (n >>> 8) & 255, n & 255];
const chunk = (type, data) => {
  const body = [...new TextEncoder().encode(type), ...data];
  return [...be32(data.length), ...body, ...be32(crc32(body))];
};

/** A PNG of one color, written with stored (uncompressed) blocks. */
function png(width, height, [r, g, b, a]) {
  const row = 1 + width * 4;
  const raw = new Uint8Array(height * row);
  for (let y = 0; y < height; y += 1) {
    for (let x = 0; x < width; x += 1) raw.set([r, g, b, a], y * row + 1 + x * 4);
  }
  const deflated = [0x78, 0x01];
  for (let at = 0; at < raw.length; at += 65535) {
    const part = raw.subarray(at, at + 65535);
    const last = at + 65535 >= raw.length;
    deflated.push(last ? 1 : 0, part.length & 255, part.length >> 8, ~part.length & 255, (~part.length >> 8) & 255);
    for (const byte of part) deflated.push(byte);
  }
  deflated.push(...be32(adler32(raw)));
  return new Uint8Array([
    137, 80, 78, 71, 13, 10, 26, 10,
    ...chunk('IHDR', [...be32(width), ...be32(height), 8, 6, 0, 0, 0]),
    ...chunk('IDAT', deflated),
    ...chunk('IEND', []),
  ]);
}

const red = () => png(16, 16, [220, 30, 30, 255]);
const blue = () => png(16, 16, [30, 30, 220, 255]);
const menu = () => [
  { id: 'show', label: 'Show' },
  { kind: 'separator' },
  { kind: 'submenu', id: 'more', label: 'More', items: [{ id: 'deep', label: 'Deep', checked: true, kind: 'check' }] },
  { role: 'copy' },
];

async function main() {
  const { check, failed } = suite();
  const { platform } = await (await fetch('targets.json', options())).json();
  const live = [];
  const create = async (settings) => {
    const handle = await api.tray.create(settings, options());
    live.push(handle);
    return handle;
  };
  try {
    await check('tray-invalid-arguments-are-rejected', async () => {
      const cases = [
        ['not a png', { icon: new Uint8Array([1, 2, 3]) }],
        ['a png wider than 256 pixels', { icon: png(300, 1, [0, 0, 0, 255]) }],
        ['a png above 256 KiB', { icon: png(256, 256, [0, 0, 0, 255]) }],
        ['a tooltip above 128 bytes', { icon: red(), tooltip: 'x'.repeat(129) }],
        ['a tooltip with a control character', { icon: red(), tooltip: 'a\nb' }],
        ['an accelerator in a tray menu', { icon: red(), menu: [{ id: 'open', label: 'Open', accelerator: 'Ctrl+O' }] }],
        ['an id twice in a tray menu', { icon: red(), menu: [{ id: 'a', label: 'A' }, { id: 'a', label: 'B' }] }],
      ];
      for (const [what, settings] of cases) {
        const error = await rejection(api.tray.create(settings, options()));
        if (error?.code !== 'INVALID_ARGUMENT') throw new Error(`${what}: expected INVALID_ARGUMENT, got ${error?.code ?? 'success'}`);
      }
    });
    if (platform === 'linux') {
      await check('tray-linux-native-operations-are-not-available', async () => {
        await expectCode(api.tray.create({ icon: red(), tooltip: 'x', menu: menu() }, options()), 'NOT_AVAILABLE');
        return 'GTK is not owned by winit; no native tray acceptance claimed';
      });
    } else {
      await check('tray-create-set-and-destroy', async () => {
        const handle = await create({ icon: red(), tooltip: 'Alef e2e tray', menu: menu() });
        await handle.setIcon(blue(), options());
        await handle.setTooltip('Alef e2e tray: changed', options());
        await handle.setTooltip('', options());
        await handle.setMenu([{ id: 'only', label: 'Only' }], options());
        await handle.setMenu([], options());
        await handle.destroy(options());
        live.splice(live.indexOf(handle), 1);
        await expectCode(handle.setTooltip('late', options()), 'NOT_FOUND');
        await expectCode(handle.destroy(options()), 'NOT_FOUND');
      });
      await check('tray-bad-updates-keep-the-tray-and-are-rejected', async () => {
        const handle = await create({ icon: red() });
        await expectCode(handle.setIcon(new Uint8Array([1, 2, 3]), options()), 'INVALID_ARGUMENT');
        await expectCode(handle.setTooltip('x'.repeat(129), options()), 'INVALID_ARGUMENT');
        await expectCode(handle.setMenu([{ id: 'a', label: 'A', accelerator: 'Ctrl+A' }], options()), 'INVALID_ARGUMENT');
        await handle.setTooltip('still here', options());
        await handle.destroy(options());
        live.splice(live.indexOf(handle), 1);
      });
      await check('tray-quit-and-about-roles-are-not-available', async () => {
        for (const role of ['quit', 'about']) {
          await expectCode(api.tray.create({ icon: red(), menu: [{ role }] }, options()), 'NOT_AVAILABLE');
        }
      });
      await check('tray-limit-is-eight-per-document', async () => {
        for (let i = 0; i < 8; i += 1) await create({ icon: red(), tooltip: `Alef e2e ${i}` });
        await expectCode(api.tray.create({ icon: red() }, options()), 'BUSY');
        while (live.length > 0) await live.pop().destroy(options());
      });
    }
    await report('MANUAL tray-icon-click-menu-and-visibility-not-tested');
  } finally {
    await check('tray-cleanup-leaves-no-icon', async () => {
      const errors = [];
      while (live.length > 0) {
        try {
          await live.pop().destroy(options());
        } catch (error) {
          errors.push(error.message);
        }
      }
      if (errors.length > 0) throw new Error(errors.join('; '));
    });
  }
  await verdict(failed());
}

guard(main);
