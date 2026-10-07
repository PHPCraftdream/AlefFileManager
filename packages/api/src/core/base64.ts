// SPDX-License-Identifier: MIT OR Apache-2.0
// Bytes in the arguments of a call (keys, salts, tags) travel as base64; the page has `btoa` and `atob`.
const CHUNK = 0x8000;

export function encodeBase64(bytes: Uint8Array): string {
  let binary = '';
  for (let at = 0; at < bytes.length; at += CHUNK) binary += String.fromCharCode(...bytes.subarray(at, at + CHUNK));
  return btoa(binary);
}

export function decodeBase64(text: string): Uint8Array<ArrayBuffer> {
  const binary = atob(text);
  const bytes = new Uint8Array(binary.length);
  for (let at = 0; at < binary.length; at += 1) bytes[at] = binary.charCodeAt(at);
  return bytes;
}
