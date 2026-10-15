import { api, guard, report, rejection, suite, verdict, sleep } from './harness.js';

const checks = suite();
const bounded = (promise, ms = 300) => {
  let timer;
  return Promise.race([promise, new Promise((_, reject) => { timer = setTimeout(() => reject(new Error(`timeout ${ms}ms`)), ms); })]).finally(() => clearTimeout(timer));
};
const call = (name, args, ms = 300) => bounded(api.call(name, args), ms);
const assertBusState = async initial => {
  const state = await call('e2e.busState');
  if (state.completed !== initial.completed || state.started !== initial.started + 1) throw new Error('unexpected bus state');
};
const consentProbe = async () => {
  await bounded(api.connect(), 8000);
  const path = await bounded(api.path.join('a', 'b'), 300);
  const platform = await call('os.platform');
  const info = await call('app.info');
  if (!path || typeof platform !== 'string' || !platform || !info || typeof info !== 'object') throw new Error('unexpected API values');
  const eventStart = performance.now();
  const remaining = () => Math.max(1, 300 - (performance.now() - eventStart));
  let received;
  let off;
  try {
    const observed = new Promise(resolve => { received = resolve; });
    off = await bounded(api.on('e2e.busEvent', payload => received(payload)), remaining());
    const emitted = await call('e2e.busEvent', undefined, remaining());
    const payload = await bounded(observed, remaining());
    if (!emitted || payload?.id !== emitted.id || performance.now() - eventStart > 300) throw new Error('event roundtrip failed');
  } finally { if (off) off(); }
  await report(`bus-consent-probe-complete path=${path} platform=${platform} info=ok event=ok`);
  await report('RESULT PASS bus-consent-probe');
};
const main = async () => {
  const targets = await bounded(fetch('targets.json').then(response => response.json()), 3000);
  if (targets.consentProbe) return consentProbe();
  const connectError = await rejection(bounded(api.connect(), 8000));
  if (connectError) throw connectError;
  await checks.check('bus-warm-api-calls', async () => {
    await bounded(api.path.join('a', 'b'), 300);
    const platform = await call('os.platform');
    const info = await call('app.info');
    if (typeof platform !== 'string' || !platform || !info || typeof info !== 'object') throw new Error('unexpected API values');
  });
  const initial = await call('e2e.busState');
  let warmOff;
  try { warmOff = await bounded(api.on('e2e.busEvent', () => {}), 300); }
  finally { if (warmOff) warmOff(); }
  const start = performance.now();
  const job = call('e2e.blockExecutor', { ms: 3000 }, 8000);
  const failure = rejection(job);
  let entered = false;
  const startupDeadline = start + 800;
  while (performance.now() < startupDeadline) {
    const remaining = startupDeadline - performance.now();
    const state = await call('e2e.busState', undefined, Math.max(1, remaining));
    if (state.started > initial.started && state.completed < state.started) { entered = true; break; }
    if (performance.now() < startupDeadline) await bounded(sleep(Math.min(20, startupDeadline - performance.now())), 30);
  }
  await checks.check('bus-executor-entered', async () => { if (!entered) throw new Error('executor did not enter blocking command'); });
  await checks.check('bus-unrelated-calls-while-blocked', async () => {
    await assertBusState(initial);
    const platform = await call('os.platform');
    const info = await call('app.info');
    const path = await bounded(api.path.join('a', 'b'), 300);
    if (typeof platform !== 'string' || !platform || !info || typeof path !== 'string' || !path) throw new Error('unexpected API result');
    await assertBusState(initial);
  });
  await checks.check('bus-event-roundtrip-while-blocked', async () => {
    await assertBusState(initial);
    let resolveEvent;
    const observed = new Promise(resolve => { resolveEvent = resolve; });
    let off;
    try {
      const roundtripStart = performance.now();
      const remaining = () => Math.max(1, 300 - (performance.now() - roundtripStart));
      off = await bounded(api.on('e2e.busEvent', payload => resolveEvent(payload)), remaining());
      const emitted = await call('e2e.busEvent', undefined, remaining());
      const payload = await bounded(observed, remaining());
      if (performance.now() - roundtripStart > 300) throw new Error('event roundtrip exceeded 300ms');
      if (emitted.id !== payload.id) throw new Error('event payload id mismatch');
    } finally { if (off) off(); }
    await assertBusState(initial);
  });
  await checks.check('bus-capacity-one-is-busy', async () => {
    await assertBusState(initial);
    const error = await rejection(call('e2e.blockExecutor', { ms: 500 }, 300));
    if (!error || error.code !== 'BUSY') throw new Error(`expected BUSY, got ${error?.code}`);
    await assertBusState(initial);
  });
  await checks.check('bus-blocking-duration-bounded', async () => {
    const error = await failure;
    if (error) throw error;
    const elapsed = performance.now() - start;
    if (elapsed < 3000 || elapsed >= 8000) throw new Error(`duration ${elapsed}ms`);
  });
  await checks.check('bus-invalid-duration-rejected', async () => {
    for (const ms of [0, 5001]) {
      const error = await rejection(call('e2e.blockExecutor', { ms }, 8000));
      if (error?.code !== 'INVALID_ARGUMENT') throw new Error(`ms=${ms}: expected INVALID_ARGUMENT, got ${error?.code}`);
    }
  });
  await checks.check('bus-negative-control-stalls', async () => {
    let response;
    try {
      await report('bus-negative-begin');
      response = await bounded(api.call('e2e.blockBus', { ms: 150 }), 5000);
    } finally {
      await report('bus-negative-end');
    }
    return response;
  });
  await verdict(checks.failed());
};
guard(main);
