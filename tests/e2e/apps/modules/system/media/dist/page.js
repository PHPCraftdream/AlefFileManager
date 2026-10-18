// The M6 media surface: the virtual camera and microphone on the page (frames into a canvas, a sine
// into numbers, a checksummed sink), and the answer of the runtime to a refused `media` right.
import { api, guard, rejection, report, sleep, suite, verdict } from './harness.js';

const { camera, microphone, audio } = api;

/** Rejects when `promise` is not settled within `ms` (every wait in this page is bounded). */
async function withDeadline(promise, ms, what) {
  let timer;
  try {
    return await Promise.race([
      promise,
      new Promise((resolve, reject) => {
        timer = setTimeout(() => reject(new Error(`timed out after ${ms} ms: ${what}`)), ms);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

/** FNV-1a 64 over bytes, as the 16-char lowercase hex the runtime answers. */
function fnv1aHex(bytes) {
  let hash = 0xcbf29ce484222325n;
  for (const byte of bytes) {
    hash ^= BigInt(byte);
    hash = (hash * 0x100000001b3n) & 0xffffffffffffffffn;
  }
  return hash.toString(16).padStart(16, '0');
}

function percentile(values, share) {
  const sorted = [...values].sort((a, b) => a - b);
  const at = Math.min(sorted.length - 1, Math.floor(sorted.length * share));
  return sorted[at];
}

/** The width, height and rate of the virtual devices, and the measured spike, live here. */
const FRAME_WIDTH = 1280;
const FRAME_HEIGHT = 720;
const FRAME_RATE = 48000;
let spike = { fps: 0, p95: Infinity, mainMs: 0, totalMs: 0, elapsed: 0 };
let everyFrameArrivedWhole = true;

async function allowed(check) {
  await check('media-camera-lists-the-virtual-device', async () => {
    const devices = await camera.list();
    if (devices.length !== 1) throw new Error(`${devices.length} cameras, expected one`);
    const device = devices[0];
    if (device.id !== 'virtual-0') throw new Error(`id ${device.id}, expected virtual-0`);
    if (device.width !== FRAME_WIDTH || device.height !== FRAME_HEIGHT) throw new Error(`shape ${device.width}x${device.height}`);
    if (!(device.fps > 0)) throw new Error(`fps ${device.fps} is not positive`);
  });

  const latencies = [];
  await check('media-camera-streams-60-frames-into-a-canvas', async () => {
    const cam = await camera.open('virtual-0', { fps: 30 });
    const context = document.getElementById('screen').getContext('2d');
    const mainMsValues = [];
    const frameMsValues = [];
    const started = Date.now();
    let seen = 0;
    let allDroppedZero = true;
    const probe = { rounds: 0, lost: 0, maxMs: 0 };
    const probeRun = (async () => {
      for (let round = 0; round < 20; round += 1) {
        const started = performance.now();
        try {
          await api.path.join('probe', String(round));
          probe.maxMs = Math.max(probe.maxMs, performance.now() - started);
          probe.rounds += 1;
        } catch {
          probe.lost += 1;
        }
        await sleep(100);
      }
    })();
    await withDeadline((async () => {
      for await (const frame of cam) {
        const frameStarted = performance.now();
        if (frame.seq !== seen + 1) throw new Error(`frame seq ${frame.seq}, expected ${seen + 1}`);
        if (frame.dropped !== 0) allDroppedZero = false;
        if (frame.width !== FRAME_WIDTH || frame.height !== FRAME_HEIGHT) throw new Error(`frame ${frame.width}x${frame.height}`);
        if (frame.data.length !== FRAME_WIDTH * FRAME_HEIGHT * 4) throw new Error(`frame payload ${frame.data.length} bytes`);
        const drawStarted = performance.now();
        context.putImageData(new ImageData(new Uint8ClampedArray(frame.data.buffer, frame.data.byteOffset, frame.data.byteLength), frame.width, frame.height), 0, 0);
        mainMsValues.push(performance.now() - drawStarted);
        latencies.push(Date.now() - frame.timestamp);
        seen += 1;
        frameMsValues.push(performance.now() - frameStarted);
        if (seen === 60) break;
      }
    })(), 45000, '60 frames');
    await probeRun;
    if (probe.rounds !== 20 || probe.lost !== 0) throw new Error(`control probe: ${probe.rounds} rounds, ${probe.lost} lost`);
    await report(`media-control-probe rounds=20 max_ms=${Math.ceil(probe.maxMs)} lost=0`);
    if (seen !== 60) throw new Error(`saw ${seen} frames, expected 60`);
    const elapsed = Date.now() - started;
    const fps = (60 * 1000) / elapsed;
    const p50 = percentile(latencies, 0.5);
    const p95 = percentile(latencies, 0.95);
    const mainMs = mainMsValues.reduce((sum, value) => sum + value, 0);
    const totalMs = frameMsValues.reduce((sum, value) => sum + value, 0);
    const stats = await cam.close();
    const dropsFromClose = stats.dropped;
    spike = { fps, p95, mainMs, totalMs, elapsed };
    everyFrameArrivedWhole = allDroppedZero;
    await report(`media-camera-stats frames=60 fps=${fps.toFixed(1)} p50_ms=${Math.round(p50)} p95_ms=${Math.round(p95)} main_ms=${Math.round(mainMs)} total_ms=${Math.round(totalMs)} drops=${dropsFromClose} elapsed_ms=${elapsed}`);
    return `fps=${fps.toFixed(1)} p50_ms=${Math.round(p50)} p95_ms=${Math.round(p95)}`;
  });

  await check('media-camera-sustains-at-least-5-fps', async () => {
    if (!(spike.fps >= 5)) throw new Error(`measured ${spike.fps.toFixed(1)} fps`);
  });
  await check('media-camera-p95-latency-stays-under-2000-ms', async () => {
    if (!(spike.p95 < 2000)) throw new Error(`p95 ${Math.round(spike.p95)} ms`);
  });
  await check('media-camera-drops-nothing-while-the-page-keeps-up', async () => {
    if (!everyFrameArrivedWhole) throw new Error('a frame was marked dropped');
  });
  await check('media-camera-main-thread-stays-under-half-the-time', async () => {
    if (!(spike.mainMs <= spike.elapsed / 2)) throw new Error(`main ${Math.round(spike.mainMs)} ms of ${spike.elapsed} ms`);
  });

  await check('media-camera-second-close-is-refused-without-touching-the-device', async () => {
    const cam = await camera.open('virtual-0', { fps: 30 });
    await withDeadline(cam.close(), 5000, 'the camera close');
    // The first close stops the producer and reports; the runtime removed the device, so a second one answers NOT_FOUND.
    const second = await rejection(withDeadline(cam.close(), 5000, 'the second camera close'));
    if (second?.code !== 'NOT_FOUND') throw new Error(`the second close answered ${second?.code ?? second?.name ?? 'it succeeded'}, expected NOT_FOUND`);
  });

  await check('media-camera-close-reports-frames-and-no-drops', async () => {
    const cam = await camera.open('virtual-0', { fps: 60 });
    let seen = 0;
    await withDeadline((async () => {
      for await (const frame of cam) {
        if (frame.dropped !== 0) throw new Error('a frame was marked dropped');
        seen += 1;
        if (seen === 5) break;
      }
    })(), 45000, '5 frames');
    if (seen !== 5) throw new Error(`saw ${seen} frames, expected 5`);
    const stats = await withDeadline(cam.close(), 5000, 'the camera close');
    if (stats.frames < 5) throw new Error(`frames ${stats.frames}`);
    if (stats.dropped !== 0) throw new Error(`dropped ${stats.dropped}`);
  });

  await check('media-microphone-lists-the-virtual-device', async () => {
    const devices = await microphone.list();
    if (devices.length !== 1) throw new Error(`${devices.length} microphones, expected one`);
    const device = devices[0];
    if (device.id !== 'virtual-0') throw new Error(`id ${device.id}, expected virtual-0`);
    if (device.rate !== FRAME_RATE) throw new Error(`rate ${device.rate}`);
    if (device.frequency !== 440) throw new Error(`frequency ${device.frequency}`);
  });

  let micStats = null;
  await check('media-microphone-chunks-carry-a-440hz-sine', async () => {
    const mic = await microphone.open('virtual-0', { frequency: 440 });
    let total = 0;
    let crossings = 0;
    let previous = null;
    await withDeadline((async () => {
      for await (const chunk of mic) {
        if (chunk.dropped !== 0) throw new Error(`dropped ${chunk.dropped}`);
        if (chunk.rate !== FRAME_RATE) throw new Error(`rate ${chunk.rate}`);
        for (const sample of chunk.data) {
          if (previous !== null && (previous > 0) !== (sample > 0)) crossings += 1;
          previous = sample;
          total += 1;
        }
        if (total >= FRAME_RATE) break;
      }
    })(), 30000, 'one second of audio');
    micStats = await withDeadline(mic.close(), 5000, 'the microphone close');
    const freq = crossings * FRAME_RATE / (2 * total);
    if (!(Math.abs(freq - 440) < 2)) throw new Error(`measured ${freq.toFixed(1)} Hz`);
  });

  await check('media-microphone-close-reports-samples', async () => {
    if (micStats.samples < FRAME_RATE) throw new Error(`samples ${micStats.samples}`);
    if (micStats.chunks < 25) throw new Error(`chunks ${micStats.chunks}`);
  });

  await check('media-audio-sink-checksum-matches', async () => {
    const sink = await audio.open();
    const samples = new Float32Array(8192);
    for (let i = 0; i < samples.length; i += 1) samples[i] = Math.fround((i % 4096) / 4096);
    for (let offset = 0; offset < samples.length; offset += 2048) {
      await withDeadline(sink.write(samples.subarray(offset, offset + 2048)), 5000, 'a sink write');
    }
    const expected = fnv1aHex(new Uint8Array(samples.buffer, samples.byteOffset, samples.byteLength));
    const stats = await withDeadline(sink.close(), 10000, 'the sink close');
    if (stats.samples !== samples.length) throw new Error(`samples ${stats.samples}, expected ${samples.length}`);
    if (stats.checksum !== expected) throw new Error(`checksum ${stats.checksum}, expected ${expected}`);
  });
}

/** The user refused `media` (or it was substituted): every call is refused, an unrelated right is not. */
async function refused(mode, check) {
  const expected = mode === 'denied' ? 'PERMISSION_DENIED' : 'NOT_AVAILABLE';
  await check(mode === 'denied' ? 'media-denied-camera-is-permission-denied' : 'media-substituted-camera-is-not-available', async () => {
    const calls = [
      ['camera.list', () => camera.list()],
      ['camera.open', () => camera.open('virtual-0')],
      ['microphone.list', () => microphone.list()],
      ['microphone.open', () => microphone.open('virtual-0')],
      ['audio.open', () => audio.open()],
    ];
    for (const [name, call] of calls) {
      const error = await rejection(call());
      if (error?.code !== expected) {
        throw new Error(`${name}: ${error?.code ?? error?.name ?? 'it succeeded'}, expected ${expected}`);
      }
    }
    await withDeadline(api.path.temp(), 5000, 'an unrelated right');
  });
}

async function main() {
  const t = await (await fetch('targets.json')).json();
  const { check, failed } = suite();
  if (t.mode === 'allowed') await allowed(check);
  else await refused(t.mode, check);
  await verdict(failed());
  await api.app.quit(0);
}

guard(main);
