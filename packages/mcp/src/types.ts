// SPDX-License-Identifier: MIT OR Apache-2.0
export const revisions = ['2025-11-25', '2025-06-18'] as const;
export type Revision = typeof revisions[number];
export type ObjectValue = Record<string, unknown>;
export interface Identity { name: string; version: string }
export interface Transport {
  send(message: unknown): Promise<void>;
  start(receive: (message: unknown) => void, end: (error?: unknown) => void): void;
  cancel?(id: string | number): void;
  close(): Promise<void>;
}
export interface RequestOptions {
  timeout?: number;
  signal?: AbortSignal;
  onProgress?: (progress: ObjectValue) => void;
}
export interface Context {
  signal: AbortSignal;
  progress(progress: number, total?: number, message?: string): Promise<void>;
}
export type Handler = (params: ObjectValue, context: Context) => unknown | Promise<unknown>;
export function object(value: unknown): value is ObjectValue {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}
export function negotiate(version: unknown): Revision {
  return revisions.find(item => item === version) ?? revisions[0];
}
export function duration(value = 30000): number {
  if (!Number.isFinite(value) || value <= 0 || value > 2147483647) throw new Error('timeout must be a positive bounded number.');
  return value;
}
