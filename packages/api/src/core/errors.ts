// SPDX-License-Identifier: MIT OR Apache-2.0
import type { ErrorCode } from '../../types/index.ts';

/** `TRANSPORT` is raised locally when the runtime did not answer with a well-formed error. */
export type AlefErrorCode = ErrorCode | 'TRANSPORT';

export interface AlefErrorOptions {
  details?: unknown;
  /** HTTP status of the failed response; 0 when there was none. */
  status?: number;
}

/** Failure of a native call or stream: the runtime's `{ code, message, details? }` or a transport fault. */
export class AlefError extends Error {
  readonly code: AlefErrorCode;
  readonly details: unknown;
  readonly status: number;

  constructor(code: AlefErrorCode, message: string, options: AlefErrorOptions = {}) {
    super(message);
    this.name = 'AlefError';
    this.code = code;
    this.details = options.details;
    this.status = options.status ?? 0;
  }
}

interface ErrorBody {
  code: string;
  message: string;
  details?: unknown;
}

const isErrorBody = (value: unknown): value is ErrorBody =>
  typeof value === 'object'
  && value !== null
  && typeof (value as ErrorBody).code === 'string'
  && typeof (value as ErrorBody).message === 'string';

/** Builds the error of a runtime-supplied `{ code, message }` body (an error response or an error frame). */
export function errorFromBody(body: unknown, status = 0): AlefError {
  if (isErrorBody(body)) {
    return new AlefError(body.code as AlefErrorCode, body.message, { details: body.details, status });
  }
  return new AlefError('TRANSPORT', status > 0 ? `Native call failed (HTTP ${status}).` : 'Native stream failed.', { status });
}

/** Reads a failed response; anything but the runtime's JSON error becomes a `TRANSPORT` error. */
export async function errorFromResponse(response: Response): Promise<AlefError> {
  let body: unknown;
  try {
    body = await response.json();
  } catch {
    body = undefined;
  }
  return errorFromBody(body, response.status);
}
