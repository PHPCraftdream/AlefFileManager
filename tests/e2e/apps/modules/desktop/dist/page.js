// Desktop modules scenario: `dialog` (answered from the script of the run, nothing is shown), `shell`
// (a desktop that only logs what it was asked, the runner reads the log) and `clipboard` (in memory,
// the clipboard of the user is never touched).
import { api, guard, rejection, same, suite, verdict } from './harness.js';

// A 2x2 RGBA PNG; the first pixel is opaque red.
const PNG = 'iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAYAAABytg0kAAAAF0lEQVR4nGP4z8DwHwwZGP7/5xKRawAAQPIGtwHS64IAAAAASUVORK5CYII=';
const pngBytes = Uint8Array.from(atob(PNG), char => char.charCodeAt(0));
const SIGNATURE = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];

const expectEqual = (what, actual, expected) => {
  if (JSON.stringify(actual) !== JSON.stringify(expected)) {
    throw new Error(`${what}: ${JSON.stringify(actual)}, expected ${JSON.stringify(expected)}`);
  }
};

const expectCode = async (what, promise, code) => {
  const error = await rejection(promise);
  if (error?.code !== code) throw new Error(`${what}: ${error?.code ?? 'it succeeded'}, expected ${code}`);
  return error;
};

const dimensions = png => {
  const view = new DataView(png.buffer, png.byteOffset, png.byteLength);
  return [view.getUint32(16), view.getUint32(20)];
};

async function main() {
  const t = await (await fetch('targets.json')).json();
  const { check, failed } = suite();
  const { dialog, shell, clipboard } = api;

  // The script answers the dialogs in order, so a refused call must not have used an answer up.
  await check('dialog-options-are-refused-before-a-dialog-is-shown', async () => {
    await expectCode('a relative start path', dialog.open({ defaultPath: 'relative/dir' }), 'INVALID_ARGUMENT');
    await expectCode('an unknown option', dialog.open({ bogus: true }), 'INVALID_ARGUMENT');
    await expectCode('equal labels', dialog.confirm({ message: 'm', okLabel: 'Same', cancelLabel: 'Same' }), 'INVALID_ARGUMENT');
    await expectCode('a message without text', dialog.message({}), 'INVALID_ARGUMENT');
    await expectCode('an extension with a dot', dialog.save({ filters: [{ name: 'x', extensions: ['.txt'] }] }), 'INVALID_ARGUMENT');
  });
  await check('dialog-open-returns-the-chosen-files', async () => {
    expectEqual('one file', await dialog.open({ title: 'Pick a note', defaultPath: t.directory }), [t.chosen]);
    expectEqual('two files', await dialog.open({ multiple: true, filters: [{ name: 'Text', extensions: ['txt'] }] }), [t.chosen, t.other]);
  });
  await check('dialog-open-cancelled-is-an-empty-list', async () => {
    expectEqual('cancelled', await dialog.open(), []);
  });
  await check('dialog-open-folder-returns-the-folder', async () => {
    expectEqual('folder', await dialog.open({ directory: true }), [t.folder]);
  });
  await check('dialog-save-returns-the-path-and-null-when-cancelled', async () => {
    expectEqual('save', await dialog.save({ defaultPath: t.saved }), t.saved);
    expectEqual('cancelled', await dialog.save(), null);
  });
  await check('dialog-message-and-confirm-answer-plainly', async () => {
    expectEqual('message', await dialog.message({ title: 'Done', message: 'All saved', kind: 'warning' }), null);
    expectEqual('confirm yes', await dialog.confirm({ message: 'Delete?', okLabel: 'Delete', cancelLabel: 'Keep' }), true);
    expectEqual('confirm no', await dialog.confirm({ message: 'Again?' }), false);
  });
  await check('dialog-answer-of-the-wrong-kind-is-an-error', async () => {
    // The last entry of the script is for `save`; this asks for `open`.
    const error = await rejection(dialog.open());
    if (!error || !/expects dialog\.save/.test(error.message)) throw new Error(`${error?.code} ${error?.message}`);
  });
  await check('a-dialog-without-a-scripted-answer-fails-instead-of-waiting', async () => {
    const error = await rejection(dialog.confirm({ message: 'nobody answers' }));
    if (!error) throw new Error('it was answered');
    if (!/no answer left/.test(error.message)) throw new Error(`unexpected error: ${error.code} ${error.message}`);
  });

  await check('shell-open-external-inside-the-scope-is-opened', async () => {
    await shell.openExternal('https://example.com/docs/guide?page=2');
  });
  await check('shell-open-external-outside-the-scope-is-denied', async () => {
    for (const url of ['https://example.com/private', 'https://evil.example/docs/x', 'http://example.com/docs/x', 'file:///etc/passwd']) {
      await expectCode(url, shell.openExternal(url), 'PERMISSION_DENIED');
    }
  });
  await check('shell-paths-in-the-scope-are-opened-shown-and-trashed', async () => {
    await shell.openPath(t.note);
    await shell.showInFolder(t.note);
    await shell.trash(t.old);
  });
  await check('shell-paths-outside-the-scope-are-denied', async () => {
    for (const command of ['openPath', 'showInFolder', 'trash']) {
      await expectCode(`${command} outside`, shell[command](t.outside), 'PERMISSION_DENIED');
    }
    for (const command of ['openPath', 'trash']) {
      await expectCode(`${command} of a relative path`, shell[command]('note.txt'), 'PERMISSION_DENIED');
    }
  });
  await check('shell-open-path-does-not-start-a-program', async () => {
    await expectCode('a program', shell.openPath(t.tool), 'PERMISSION_DENIED');
  });
  await check('shell-a-path-that-is-not-there-is-not-found', async () => {
    for (const command of ['openPath', 'showInFolder', 'trash']) {
      await expectCode(`${command} of nothing`, shell[command](t.gone), 'NOT_FOUND');
    }
  });

  await check('clipboard-text-passes-whatever-its-size', async () => {
    for (const text of ['plain', 'Привет, мир — שלום 🙂', '', '\u{FEFF}with a mark', 'x'.repeat(1024 * 1024), 'Ж'.repeat(400 * 1024)]) {
      await clipboard.writeText(text);
      const back = await clipboard.readText();
      if (back !== text) throw new Error(`${text.length} characters came back as ${back.length}`);
    }
  });
  await check('clipboard-html-passes-and-text-replaces-it', async () => {
    await clipboard.writeHtml('<b>bold</b> и <i>italic</i>');
    expectEqual('html', await clipboard.readHtml(), '<b>bold</b> и <i>italic</i>');
    expectEqual('text beside html', await clipboard.readText(), '');
    await clipboard.writeText('now text');
    expectEqual('html after text', await clipboard.readHtml(), '');
  });
  await check('clipboard-image-passes-as-png-and-text-replaces-it', async () => {
    expectEqual('no image yet', await clipboard.readImage(), null);
    await clipboard.writeImage(pngBytes);
    const first = await clipboard.readImage();
    if (!first || !same(Array.from(first.subarray(0, 8)), SIGNATURE)) throw new Error('what came back is not a PNG');
    expectEqual('size', dimensions(first), [2, 2]);
    await clipboard.writeImage(first);
    const second = await clipboard.readImage();
    if (!second || !same(first, second)) throw new Error('a PNG that went in and out again changed');
    expectEqual('text beside an image', await clipboard.readText(), '');
    await clipboard.writeText('text again');
    expectEqual('image after text', await clipboard.readImage(), null);
  });
  await check('clipboard-refuses-what-is-not-an-image-or-not-text', async () => {
    await clipboard.writeText('kept');
    await expectCode('garbage', clipboard.writeImage(new Uint8Array([1, 2, 3])), 'INVALID_ARGUMENT');
    await expectCode('a truncated PNG', clipboard.writeImage(pngBytes.slice(0, 30)), 'INVALID_ARGUMENT');
    expectEqual('the clipboard is as it was', await clipboard.readText(), 'kept');
    await expectCode('bytes that are not text', api.call('clipboard.writeText', null, { body: new Uint8Array([0x66, 0xff, 0xfe]) }), 'INVALID_ARGUMENT');
    await expectCode('no body', api.call('clipboard.writeText', null), 'INVALID_ARGUMENT');
  });

  await check('notification-is-shown-and-its-text-and-icon-are-checked', async () => {
    const { notification } = api;
    await notification.show({ title: 'Done', body: 'All saved\nin two places' });
    await notification.show({ title: 'With an icon', icon: t.icon });
    for (const [what, options, code] of [
      ['an empty title', { title: '' }, 'INVALID_ARGUMENT'],
      ['a title with a line break', { title: 'a\nb' }, 'INVALID_ARGUMENT'],
      ['an unknown field', { title: 't', sound: 'ding' }, 'INVALID_ARGUMENT'],
      ['an icon outside the read scope', { title: 't', icon: t.outside }, 'PERMISSION_DENIED'],
      ['an icon that is not there', { title: 't', icon: t.gone }, 'NOT_FOUND'],
    ]) {
      await expectCode(what, notification.show(options), code);
    }
  });

  await verdict(failed());
}

guard(main);
