// Window module scenario (docs/stages/m2-desktop.md, "Приёмка"): geometry in px, %work and %screen,
// the second window, min/max, events, the close request, destroying a window, the label rules.
// Sizes are compared with the displays reported by `screen`; the OS may add a pixel or two.
import { api, guard, rejection, report, sleep, suite, verdict } from './harness.js';

const near = (actual, expected, tolerance, what) => {
  if (!(Math.abs(actual - expected) <= tolerance)) {
    throw new Error(`${what}: ${actual}, expected ${expected} (+-${tolerance})`);
  }
};

/** Runs `body` until it stops throwing (and returns something truthy); fails with its last complaint. */
async function eventually(body, ms, what) {
  const deadline = performance.now() + ms;
  let last = 'not yet';
  for (;;) {
    try {
      const result = await body();
      if (result) return result;
    } catch (error) {
      last = error.message;
    }
    if (performance.now() > deadline) throw new Error(`timed out waiting for ${what}: ${last}`);
    await sleep(40);
  }
}

const labels = async () => (await api.window.all()).map(window => window.label).sort();
const gone = label => async () => !(await labels()).includes(label);
const armed = async window => (await window.state()).title === 'armed';

async function main() {
  const { check, failed } = suite();
  const self = await api.window.current();
  const monitors = await api.screen.monitors();
  const primary = monitors.find(monitor => monitor.primary) ?? monitors[0];
  const { workArea: work, bounds } = primary;
  let child;

  await check('declared-windows-are-open', async () => {
    if (self.label !== 'main') throw new Error(`the window of this document is ${self.label}`);
    const open = await labels();
    if (JSON.stringify(open) !== JSON.stringify(['main', 'side'])) throw new Error(`open windows: ${open}`);
  });
  await check('screen-describes-the-displays', async () => {
    if (monitors.length === 0) throw new Error('no display');
    const flagged = monitors.filter(monitor => monitor.primary).length;
    if (flagged !== 1) throw new Error(`${flagged} primary displays`);
    for (const monitor of monitors) {
      const { bounds: whole, workArea: usable, scaleFactor } = monitor;
      if (!(scaleFactor > 0 && whole.width > 0 && whole.height > 0)) throw new Error(`bad display ${JSON.stringify(monitor)}`);
      const inside = usable.x >= whole.x - 1 && usable.y >= whole.y - 1
        && usable.x + usable.width <= whole.x + whole.width + 1 && usable.y + usable.height <= whole.y + whole.height + 1;
      if (!inside) throw new Error(`the work area leaves the display: ${JSON.stringify(monitor)}`);
    }
    await report(`screen ${JSON.stringify(monitors)}`);
  });
  await check('size-in-percent-of-the-work-area', async () => {
    const state = await self.state();
    near(state.width, 0.7 * work.width, 2, 'width of 70%work');
    near(state.height, 0.6 * work.height, 2, 'height of 60%work');
    return `${state.width}x${state.height} in ${work.width}x${work.height}`;
  });
  await check('the-window-is-centred-in-the-work-area', async () => {
    const state = await self.state();
    // The frame around the client area is a little larger than it; the tolerance covers it.
    near(state.x + state.width / 2, work.x + work.width / 2, 40, 'horizontal centre');
    near(state.y + state.height / 2, work.y + work.height / 2, 60, 'vertical centre');
  });
  await check('the-size-and-position-of-a-declared-window', async () => {
    const side = (await api.window.all()).find(window => window.label === 'side');
    await eventually(() => armed(side), 60000, 'the side window to load its document');
    const state = await side.state();
    near(state.width, 480, 2, 'width in pixels');
    near(state.height, 320, 2, 'height in pixels');
    near(state.x, work.x + 0.1 * work.width, 3, 'x of 10%work');
    near(state.y, work.y + 0.1 * work.height, 3, 'y of 10%work');
    if (state.title !== 'armed' && state.title !== 'Side window') throw new Error(`title ${state.title}`);
    if (state.resizable) throw new Error('the side window must not be resizable');
  });

  await check('create-opens-a-window-with-percent-size-and-an-explicit-position', async () => {
    child = await api.window.create({
      label: 'child', url: '/quiet.html', width: '50%screen', height: '40%screen',
      minWidth: 300, minHeight: 200, position: { x: work.x + 60, y: work.y + 80 },
    });
    if (child.label !== 'child') throw new Error(`label ${child.label}`);
    await eventually(async () => {
      const state = await child.state();
      near(state.width, 0.5 * bounds.width, 2, 'width of 50%screen');
      near(state.height, 0.4 * bounds.height, 2, 'height of 40%screen');
      near(state.x, work.x + 60, 3, 'x');
      near(state.y, work.y + 80, 3, 'y');
      return true;
    }, 8000, 'the size and position of the new window');
    await eventually(() => armed(child), 60000, 'the new window to load its document');
    // The system may move a window that is shown (into the work area): what counts is where it stays.
    await sleep(500);
    const shown = await child.state();
    near(shown.x, work.x + 60, 3, 'x once shown');
    near(shown.y, work.y + 80, 3, 'y once shown');
    if (!(await labels()).includes('child')) throw new Error('the new window is not listed');
  });
  await check('set-size-and-set-position-accept-percentages', async () => {
    await child.setSize('40%work', 260);
    await child.setPosition('10%work', '20%work');
    await eventually(async () => {
      const state = await child.state();
      near(state.width, 0.4 * work.width, 2, 'width of 40%work');
      near(state.height, 260, 2, 'height');
      near(state.x, work.x + 0.1 * work.width, 3, 'x of 10%work');
      near(state.y, work.y + 0.2 * work.height, 3, 'y of 20%work');
      return true;
    }, 8000, 'the new size and position');
  });
  await check('minimum-and-maximum-size-are-enforced', async () => {
    await child.setMaxSize(500, 400);
    await child.setSize(900, 900);
    await eventually(async () => {
      const state = await child.state();
      near(state.width, 500, 2, 'width above the maximum');
      near(state.height, 400, 2, 'height above the maximum');
      return true;
    }, 8000, 'the maximum size to hold');
    await child.setMaxSize();
    await child.setMinSize(350, 250);
    await child.setSize(100, 100);
    await eventually(async () => {
      const state = await child.state();
      near(state.width, 350, 2, 'width below the minimum');
      near(state.height, 250, 2, 'height below the minimum');
      return true;
    }, 8000, 'the minimum size to hold');
    await child.setMinSize();
  });
  await check('maximize-and-restore-change-the-state', async () => {
    await child.maximize();
    await eventually(async () => (await child.state()).maximized, 8000, 'maximized');
    await child.restore();
    await eventually(async () => !(await child.state()).maximized, 8000, 'restored');
  });
  await check('events-moved-and-resized-name-their-window', async () => {
    const moved = [];
    const resized = [];
    const strays = [];
    const stops = [
      await child.on('moved', event => moved.push(event)),
      await child.on('resized', event => resized.push(event)),
      await self.on('resized', event => strays.push(event)),
      await self.on('moved', event => strays.push(event)),
    ];
    await child.setPosition(200, 150);
    await child.setSize(420, 310);
    await eventually(async () => {
      const [position, size] = [moved.at(-1), resized.at(-1)];
      if (!position || !size) throw new Error(`moved ${moved.length}, resized ${resized.length}`);
      if (position.label !== 'child' || size.label !== 'child') throw new Error('an event names another window');
      near(position.x, 200, 3, 'x of the moved event');
      near(position.y, 150, 3, 'y of the moved event');
      near(size.width, 420, 2, 'width of the resized event');
      near(size.height, 310, 2, 'height of the resized event');
      return true;
    }, 8000, 'the moved and resized events');
    for (const stop of stops) stop();
    if (strays.length > 0) throw new Error(`events of the child reached the handlers of main: ${JSON.stringify(strays)}`);
  });
  await check('title-zoom-hide-and-show', async () => {
    await child.setTitle('Renamed');
    await eventually(async () => (await child.state()).title === 'Renamed', 8000, 'the new title');
    await child.setZoom(1.5);
    await eventually(async () => Math.abs((await child.state()).zoom - 1.5) < 0.01, 8000, 'the zoom');
    const refused = await rejection(child.setZoom(10));
    if (refused?.code !== 'INVALID_ARGUMENT') throw new Error(`zoom 10: ${refused?.code}`);
    await child.hide();
    await eventually(async () => (await child.state()).visible !== true, 8000, 'hidden');
    await child.show();
    await eventually(async () => (await child.state()).visible !== false, 8000, 'shown');
  });
  await check('label-and-url-rules-are-enforced', async () => {
    const same = await rejection(api.window.create({ label: 'child', url: '/quiet.html', width: 300, height: 200 }));
    if (same?.code !== 'ALREADY_EXISTS') throw new Error(`a label in use: ${same?.code}`);
    const foreign = await rejection(api.window.create({ label: 'other', url: '//evil.example/x', width: 300, height: 200 }));
    if (foreign?.code !== 'INVALID_ARGUMENT') throw new Error(`another host: ${foreign?.code}`);
    const nobody = await rejection(new api.AppWindow('nobody').state());
    if (nobody?.code !== 'NOT_FOUND') throw new Error(`an unknown label: ${nobody?.code}`);
    const foreignClose = await rejection(new api.AppWindow('side').on('close-requested', () => {}));
    if (foreignClose?.code !== 'INVALID_ARGUMENT') throw new Error(`close-requested of another window: ${foreignClose?.code}`);
  });

  await check('files-dropped-on-a-window-are-announced-and-become-readable', async () => {
    const { drop } = await (await fetch('targets.json')).json();
    const heard = [];
    const off = await self.on('file-drop', event => heard.push(event));
    const readable = async path => (await rejection(api.call('e2e.fsRead', { target: path }))) === null;
    try {
      if (await readable(drop.file)) throw new Error('the file was readable before the drop');
      await api.call('e2e.dropFiles', { paths: [drop.file, drop.folder, drop.missing] });
      await eventually(() => heard.length > 0, 10000, 'the file-drop event');
      const [event] = heard;
      if (event.label !== 'main') throw new Error(`the event names the window ${event.label}`);
      if (JSON.stringify(event.paths) !== JSON.stringify([drop.file, drop.folder])) throw new Error(`paths ${JSON.stringify(event.paths)}`);
      for (const path of [drop.file, drop.folder, drop.inside]) {
        if (!(await readable(path))) throw new Error(`${path} is not readable after the drop`);
      }
      for (const path of [drop.sibling, drop.missing]) {
        if (await readable(path)) throw new Error(`${path} became readable`);
      }
    } finally {
      off();
    }
  });
  await check('a-document-can-refuse-and-then-allow-the-close-of-its-window', async () => {
    const side = (await api.window.all()).find(window => window.label === 'side');
    await eventually(() => armed(side), 60000, 'the side window to arm its close handler');
    const started = performance.now();
    await side.setTitle('close-now');
    await eventually(gone('side'), 30000, 'the side window to close after its handler allowed it');
    const took = performance.now() - started;
    if (took < 400) throw new Error(`the window closed after ${Math.round(took)} ms: the first request was not refused`);
    if (!(await labels()).includes('main')) throw new Error('main went away with the side window');
  });
  await check('an-unanswered-close-request-closes-the-window-after-the-limit', async () => {
    const started = performance.now();
    await child.close();
    await eventually(gone('child'), 20000, 'the window to close although its document never answered');
    const took = performance.now() - started;
    if (took < 2500 || took > 12000) throw new Error(`closed after ${Math.round(took)} ms, the limit is 3000 ms`);
    return `${Math.round(took)} ms`;
  });
  await check('destroy-does-not-wait-for-the-document', async () => {
    const doomed = await api.window.create({ label: 'doomed', url: '/quiet.html', width: 300, height: 200 });
    await eventually(() => armed(doomed), 60000, 'the new window to load its document');
    const started = performance.now();
    await doomed.destroy();
    await eventually(gone('doomed'), 2500, 'the window to go away');
    return `${Math.round(performance.now() - started)} ms`;
  });

  await verdict(failed());
  // The process ends with its last window.
  await self.destroy();
}

guard(main);
