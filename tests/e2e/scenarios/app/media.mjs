// SPDX-License-Identifier: MIT OR Apache-2.0
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

// The raw 1280x720 stream saturates the page transport of the macOS runners (1.4-3.1 fps, control
// calls delayed by 340-640 ms: docs/stages/m6-media.md, task M6.3). The scheduler then stretches
// single polls of the bridge glue past the watchdog's 50 ms (seen: bridge.pump 141 ms, with
// hundreds of stream acks in flight). Those stalls are the symptom of that saturation, not a
// separate defect: they are tolerated ONLY in the allowed run on macOS, capped below 1 s and
// counted, so a real block still fails. Remove this allowance when M6.3 lands.
const SATURATED_TRANSPORT = process.platform === 'darwin'
  ? ['bridge.pump', 'bridge.request'].map(kind => ({ kind, minMs: 50, maxMs: 1000, count: 100 }))
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
          expectedBusStalls: mode === 'allowed' ? SATURATED_TRANSPORT : [],
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
