// The side window declared in the manifest. It answers the close requests of its own window: the
// first one is refused, the second one allowed. The main page starts the exchange by retitling this
// window (`close-now`); a title is the one thing both documents can see. That the window is gone
// after the second request is for the main page to see: this document does not outlive it.
import { api, guard, report, sleep, suite } from './harness.js';

async function untilTitle(side, wanted, ms) {
  const deadline = performance.now() + ms;
  while ((await side.state()).title !== wanted) {
    if (performance.now() > deadline) throw new Error(`timed out waiting for the title ${wanted}`);
    await sleep(50);
  }
}

async function main() {
  const { check } = suite();
  const side = await api.window.current();
  let requests = 0;
  let firstRequest;
  const first = new Promise(resolve => { firstRequest = resolve; });
  await side.on('close-requested', async event => {
    requests += 1;
    await report(`side close-request ${requests} label=${event.label}`);
    if (requests === 1) {
      event.preventDefault();
      firstRequest();
    }
  });
  await side.setTitle('armed');
  await report('side-armed');
  await untilTitle(side, 'close-now', 60000);

  await check('close-requested-can-be-prevented', async () => {
    await side.close();
    await Promise.race([first, sleep(5000).then(() => { throw new Error('the first close request never came'); })]);
    await sleep(500);
    const state = await side.state();
    if (state.label !== 'side') throw new Error(`the window is gone: ${JSON.stringify(state)}`);
    const labels = (await api.window.all()).map(window => window.label);
    if (!labels.includes('side')) throw new Error(`the window left the list: ${labels}`);
  });
  await report('side second-close');
  void side.close();
}

guard(main);
