// SPDX-License-Identifier: MIT OR Apache-2.0
import { call } from '../core/transport.ts';
import type { Cancelable } from '../core/events.ts';

export type LogLevel = 'debug' | 'info' | 'warn' | 'error';

export interface LogRecord {
  time: string;
  level: LogLevel;
  message: string;
  fields: Record<string, unknown>;
  dropped?: number;
}

export interface CrashInfo {
  id: string;
  time: string;
  size: number;
}

export interface LogReadOptions extends Cancelable {
  limit?: number;
  level?: LogLevel;
}

const write = (level: LogLevel, message: string, fields?: Record<string, unknown>): Promise<void> =>
  call<void>(`log.${level}`, fields === undefined ? { message } : { message, fields });

export const log = {
  debug: (message: string, fields?: Record<string, unknown>): Promise<void> => write('debug', message, fields),
  info: (message: string, fields?: Record<string, unknown>): Promise<void> => write('info', message, fields),
  warn: (message: string, fields?: Record<string, unknown>): Promise<void> => write('warn', message, fields),
  error: (message: string, fields?: Record<string, unknown>): Promise<void> => write('error', message, fields),
  read: (options: LogReadOptions = {}): Promise<LogRecord[]> => {
    const { signal, limit, level } = options;
    const args: { limit?: number; level?: LogLevel } = {};
    if (limit !== undefined) args.limit = limit;
    if (level !== undefined) args.level = level;
    return call<LogRecord[]>('log.read', args, { signal });
  },
  crashes: (): Promise<CrashInfo[]> => call<CrashInfo[]>('log.crashes', {}),
  crash: (id: string): Promise<string> => call<string>('log.crash', { id }),
};
