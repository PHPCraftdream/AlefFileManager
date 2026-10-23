// SPDX-License-Identifier: MIT OR Apache-2.0
import net from 'node:net';
import { field, prepareSite, startApp, verdictOf } from '../../lib.mjs';

const withoutOpenGl = lines => process.platform === 'win32'
  && lines.some(line => line.includes('no software rendering context'));

function bounded(operation, ms, what) {
  let timer;
  return Promise.race([
    operation,
    new Promise((_resolve, reject) => { timer = setTimeout(() => reject(new Error(`timed out waiting for ${what}`)), ms); }),
  ]).finally(() => clearTimeout(timer));
}

async function freePort() {
  const server = net.createServer();
  try {
    await bounded(new Promise((resolve, reject) => {
      server.once('error', reject);
      server.listen(0, '127.0.0.1', resolve);
    }), 5000, 'a free port');
    return server.address().port;
  } finally {
    await bounded(new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve())), 5000, 'port probe cleanup');
  }
}

async function ask(port, path) {
  const response = await fetch(`http://127.0.0.1:${port}${path}`, { signal: AbortSignal.timeout(5000) });
  return { status: response.status, text: await response.text() };
}

const COMMON = ['launch-starts-undecided', 'launch-same-mode-is-a-no-op', 'launch-keeps-arguments'];
const WINDOW = ['launch-starts-undecided', 'launch-child-is-window', 'launch-child-has-declared-window',
  'launch-child-has-no-stdio', 'launch-child-keeps-arguments'];

export function launchModesScenarios({ exe, verbose, timeoutMs }) {
  const waitMs = Math.min(timeoutMs, 120000);
  async function run(mode, signal = false) {
    const port = mode === 'foreground' ? await freePort() : 1;
    const name = `launch-modes-${mode}${signal ? '-signal' : ''}`;
    // Like service-signal, Node's Windows kill is not a graceful terminal event.
    // This harness has no console-event injector; keep testing the request path instead.
    const deliverSignal = signal && process.platform !== 'win32';
    const site = prepareSite(name, 'modules/app/launch-modes', { replacements: { PORT: String(port) } });
    const tail = ['--mode', mode, ...(mode === 'foreground' ? ['--port', String(port)] : [])];
    const running = startApp({ exe, args: ['--app', site, '--', ...tail], verbose,
      env: mode === 'undecided' ? { ALEF_E2E_LAUNCH_TIMEOUT_MS: '50' } : {} });
    const problems = [];
    try {
      if (mode !== 'undecided') {
        await running.waitFor(line => line.includes('ALEF_E2E RESULT'), waitMs, 'launch verdict');
        const result = verdictOf(running.lines);
        if (result?.[1] !== 'PASS') problems.push(`verdict ${result?.[1]}: ${result?.[2]}`);
        const done = new Set(running.lines.map(line => / check (\S+) ok /.exec(line)?.[1]));
        for (const check of mode === 'window' ? WINDOW : COMMON) {
          if (!done.has(check)) problems.push(`missing check ${check}`);
        }
        if (mode === 'foreground') {
          await running.waitFor(line => line === 'ALEF_E2E launch-foreground-ready', waitMs, 'foreground listener');
          const pong = await ask(port, '/ping');
          if (pong.status !== 200 || pong.text !== 'pong') problems.push(`/ping: ${pong.status} ${pong.text}`);
          if (deliverSignal) process.kill(running.pid, 'SIGTERM');
          else {
            if (signal) console.log('    skipped: foreground SIGTERM delivery on Windows: Node process.kill terminates the process without invoking its signal handler; no terminal-event injector in this harness. Testing /quit, exit 5 and before-quit instead; Ctrl+C/Ctrl+Break require manual coverage.');
            const bye = await ask(port, '/quit');
            if (bye.status !== 200 || bye.text !== 'bye') problems.push(`/quit: ${bye.status} ${bye.text}`);
          }
        }
      }
      const exit = await running.waitForExit(waitMs);
      if (!(await running.waitForClosed(10000))) problems.push('runtime or descendant still holds the output pipes');
      if (withoutOpenGl(running.lines)) {
        console.log('    skipped: this machine has no OpenGL driver for a process without a window');
        problems.length = 0;
        return { problems, lines: running.lines, site };
      }
      const expected = mode === 'cli' ? 7 : mode === 'window' ? 9 : mode === 'undecided' ? 2 : deliverSignal ? 143 : 5;
      if (exit?.code !== expected) problems.push(`parent exit ${exit?.code}, expected ${expected}`);
      // Drain the stdout pipe first: the exact match loses the tail of a stream still in flight.
      if (mode === 'cli' && (!(await running.stdoutDrained(10000)) || running.stdout().toString() !== 'ALEF_LAUNCH_CLI selected cli\n')) problems.push('CLI stdout did not match exactly');
      if (mode === 'foreground' && !running.lines.includes('ALEF_E2E launch-before-quit')) problems.push('foreground did not hear before-quit');
      if (mode === 'window') {
        const ready = running.lines.filter(line => line.startsWith('ALEF_READY '));
        const pids = ready.map(line => field(line, 'pid'));
        if (pids.length !== 2 || pids[0] !== running.pid || !(pids[1] > 0) || pids[1] === running.pid) {
          problems.push(`expected distinct parent and window ALEF_READY pids: ${JSON.stringify(pids)}`);
        }
        if (!running.lines.includes('ALEF_E2E launch-decision mode=window raw=["--mode","window"]')
          || !running.lines.includes('ALEF_E2E launch-child raw=["--mode","window"]')) problems.push('parent and child did not report unchanged arguments');
      }
      if (mode === 'undecided' && !running.stderr().toString().includes('launch mode selection expired')) problems.push('timeout diagnostic missing from stderr');
    } catch (error) {
      if (withoutOpenGl(running.lines)) console.log('    skipped: this machine has no OpenGL driver for a process without a window');
      else problems.push(error.message);
    } finally {
      running.stop();
      if (!(await running.waitForExit(10000))) problems.push('runtime did not exit within the cleanup bound');
      if (!(await running.waitForClosed(10000))) problems.push('runtime descendants did not release pipes within the cleanup bound');
    }
    return { problems, lines: running.lines, site };
  }
  return {
    async 'launch-modes'() {
      const problems = [];
      const lines = [];
      for (const [mode, signal] of [['cli', false], ['foreground', false], ['foreground', true], ['window', false], ['undecided', false]]) {
        const result = await run(mode, signal);
        problems.push(...result.problems.map(problem => `${mode}${signal ? '-signal' : ''}: ${problem}`));
        lines.push(...result.lines);
      }
      return { problems, lines };
    },
  };
}

// The `media` scenario (docs/stages/m6-media.md), apart from modules.mjs (the file limit).
// The media module (docs/stages/m6-media.md) on the virtual devices: the allowed run measures the
// spike (60 frames of 1280x720@30 into a canvas, a 440 Hz sine, a checksummed sink); the denied and
// substituted runs answer PERMISSION_DENIED and NOT_AVAILABLE without touching a device.
const MEDIA_ALLOWED_CHECKS = [
  'media-camera-lists-the-virtual-device',
  'media-camera-streams-60-frames-into-a-canvas',
  'media-camera-sustains-the-platform-fps-floor',
  'media-camera-p95-latency-stays-under-the-platform-limit',
  'media-camera-sequence-matches-the-drop-accounting',
  'media-camera-main-thread-stays-under-half-the-time',
  'media-camera-second-close-is-refused-without-touching-the-device',
  'media-camera-close-reports-frames-and-accounting',
  'media-microphone-lists-the-virtual-device',
  'media-microphone-chunks-carry-a-440hz-sine',
  'media-microphone-close-reports-samples',
  'media-audio-sink-checksum-matches',
];
const MEDIA_MODE_CHECKS = {
  allowed: MEDIA_ALLOWED_CHECKS,
  denied: ['media-denied-camera-is-permission-denied'],
  substituted: ['media-substituted-camera-is-not-available'],
};

// The raw 1280x720 stream keeps the 3-vCPU Intel macOS runner saturated (60 frames take 4-18 s). The
// scheduler then stretches a single poll of the bridge glue past the watchdog's 50 ms (seen: bridge.pump
// 68 ms and 141 ms; never on Linux and Windows). That is CPU starvation, not a blocked bus: it is
// tolerated ONLY in the allowed run on macOS, capped at 250 ms and counted, so a real block still fails.
const SATURATED_RUNNER = process.platform === 'darwin'
  ? ['bridge.pump', 'bridge.request'].map(kind => ({ kind, minMs: 50, maxMs: 250, count: 8 }))
  : [];

export function mediaScenarios({ drive }) {
  return {
    // The virtual camera, microphone and audio sink: the judge keeps the measured spike of the
    // allowed run in the CI log (the `media-camera-stats` line the page reports).
    async media() {
      const runs = [];
      const problems = [];
      for (const [mode, expected] of Object.entries(MEDIA_MODE_CHECKS)) {
        const decision = mode === 'allowed' ? '*=allow'
          : mode === 'denied' ? 'media.camera=deny;media.microphone=deny;audio=deny;*=allow'
          : 'media.camera=substitute;media.microphone=substitute;audio=substitute;*=allow';
        const result = await drive({
          name: `media-${mode}`, app: 'modules/system/media', replacements: { ID: `org.alef.e2e.modules.media.${mode}` },
          targets: { mode },
          env: { ALEF_E2E_CONSENT: decision },
          expectedChecks: expected,
          expectedBusStalls: mode === 'allowed' ? SATURATED_RUNNER : [],
          judge: async lines => {
            for (const line of lines.filter(candidate => candidate.includes('media-camera-stats'))) {
              console.log(`    ${line}`);
            }
            return [];
          },
        });
        runs.push(...result.lines);
        problems.push(...result.problems.map(problem => `${mode}: ${problem}`));
      }
      return { problems, lines: runs };
    },
  };
}
