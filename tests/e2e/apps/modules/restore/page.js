// Window restore: the runner starts this application several times in a row (`--step=...`) and, between
// the runs, reads or rewrites the file in which the runtime remembers the window. Each run checks where
// its window opened and, in the first ones, leaves the window somewhere else for the next run.
import { api, guard, report, sleep, suite, verdict } from './harness.js';

async function eventually(body, ms, what) {
  const deadline = performance.now() + ms;
  let last;
  for (;;) {
    try {
      const value = await body();
      if (value) return value;
    } catch (error) {
      last = error;
    }
    if (performance.now() > deadline) throw new Error(`timed out waiting for ${what}${last ? `: ${last.message}` : ''}`);
    await sleep(25);
  }
}

const near = (actual, expected, tolerance) => typeof actual === 'number' && Math.abs(actual - expected) <= tolerance;

const describe = state => `${state.width}x${state.height} at ${state.x},${state.y}${state.maximized ? ' maximized' : ''}`;

async function main() {
  const { tolerance } = await (await fetch('targets.json')).json();
  const { check, failed } = suite();
  const { step } = (await api.app.args()).parsed;
  const win = await api.window.current();
  await report(`path appData ${await api.path.appData()}`);
  const sized = (width, height) => async () => {
    const state = await win.state();
    return near(state.width, width, tolerance) && near(state.height, height, tolerance) ? state : null;
  };

  if (step === 'save') {
    await check('the-first-run-opens-the-window-as-declared', async () => {
      const state = await eventually(sized(800, 600), 20000, 'the declared size');
      return describe(state);
    });
    await check('a-window-moved-and-resized-is-left-somewhere-else', async () => {
      await win.setSize(777, 555);
      await win.setPosition(150, 130);
      const state = await eventually(async () => {
        const now = await win.state();
        return near(now.width, 777, tolerance) && near(now.height, 555, tolerance) && near(now.x, 150, 80) && near(now.y, 130, 80) ? now : null;
      }, 20000, 'the new size and position');
      return describe(state);
    });
  } else if (step === 'reopen') {
    await check('the-window-opens-where-it-was-left', async () => {
      const state = await eventually(sized(777, 555), 20000, '777x555');
      if (!near(state.x, 150, 80) || !near(state.y, 130, 80)) throw new Error(`position ${state.x},${state.y}, expected about 150,130`);
      return describe(state);
    });
    await check('a-maximized-window-is-remembered-as-maximized', async () => {
      await win.maximize();
      await eventually(async () => (await win.state()).maximized, 20000, 'the window to be maximized');
    });
  } else if (step === 'maximized') {
    await check('the-window-opens-maximized-again-and-restores-to-its-old-size', async () => {
      await eventually(async () => (await win.state()).maximized, 20000, 'the window to open maximized');
      await win.restore();
      const state = await eventually(async () => {
        const now = await win.state();
        return !now.maximized && near(now.width, 777, tolerance) && near(now.height, 555, tolerance) ? now : null;
      }, 20000, 'the old size after restore');
      return describe(state);
    });
  } else if (step === 'gone') {
    await check('a-place-on-no-display-gives-the-size-and-a-reachable-position', async () => {
      const state = await eventually(sized(700, 500), 20000, '700x500');
      const monitors = await api.screen.monitors();
      const centre = { x: state.x + state.width / 2, y: state.y + state.height / 2 };
      const inside = monitors.some(({ workArea: a }) => centre.x >= a.x && centre.x < a.x + a.width && centre.y >= a.y && centre.y < a.y + a.height);
      if (!inside) throw new Error(`the window ${describe(state)} is on no display: ${JSON.stringify(monitors.map(m => m.workArea))}`);
      return describe(state);
    });
  } else if (step === 'damaged') {
    await check('a-damaged-state-file-leaves-the-window-as-declared', async () => {
      const state = await eventually(sized(800, 600), 20000, 'the declared size');
      return describe(state);
    });
  } else {
    throw new Error(`unknown step ${step}`);
  }
  await verdict(failed());
  await api.app.quit(0);
}

guard(main);
