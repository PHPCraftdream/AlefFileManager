// SPDX-License-Identifier: MIT OR Apache-2.0
export const PLATFORM_VERSION_RELATION = 'workspace-version-placeholder';

export const PLATFORMS = Object.freeze([
  Object.freeze({ platform: 'win32', arch: 'x64', suffix: 'win32-x64', binary: 'alef.exe', ciName: 'windows-x64' }),
  Object.freeze({ platform: 'darwin', arch: 'arm64', suffix: 'darwin-arm64', binary: 'alef', ciName: 'macos-arm64' }),
  Object.freeze({ platform: 'darwin', arch: 'x64', suffix: 'darwin-x64', binary: 'alef', ciName: 'macos-x64' }),
  Object.freeze({ platform: 'linux', arch: 'x64', suffix: 'linux-x64', binary: 'alef', ciName: 'linux-x64' }),
]);

export function packageName(entry) {
  return `@alef-runtime/runtime-${entry.suffix}`;
}
