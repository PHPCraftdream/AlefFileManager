// SPDX-License-Identifier: MIT OR Apache-2.0
// The canonical e2e groups of Windows and macOS, one runner process per group so an earlier
// failure does not hide the later groups. Windows runs them as separate ci.yml steps; macOS runs
// them through scripts/ci/e2e/os.mjs. The groups are total: every scenario of tests/e2e/run.mjs
// is in exactly one group; platform-only scenarios stay in a group and skip themselves with a
// reason. The test keeps the Windows steps and this list identical.
export function expandGroup(group) {
  return group === 'core' ? 'core,bus' : group;
}

export const E2E_GROUPS = [
  'console,service,service-signal,arguments,launch-modes,mcp-stdio,startup,manifest',
  'core,bus',
  'window',
  'csp',
  'permissions',
  'induced,dev',
  'consent,narrowing,ask,cancel,decisions',
  'apps-ui',
  'app,quit,relaunch,instance,restore,autostart,deeplink,devflow,devtools',
  'system,desktop,menu,context-menu,tray,shortcut,shortcut-denied,shortcut-substitute,cli,cli-substitute,cli-commands',
  'fs,fs-substitute,sqlite,sqlite-substitute,store,log,crypto,webcrypto,secrets,secrets-substitute,secrets-denied',
  'http,http-substitute,socket,socket-substitute,websocket,websocket-substitute,serve,serve-substitute,mcp,ai,ai-threat,media',
  'origin,webcrypto-undeclared,webcrypto-substitute,package,updater',
];
