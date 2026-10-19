// SPDX-License-Identifier: MIT OR Apache-2.0
// The `media` segment: cameras, microphones and the audio sink (paired json header + binary payload
// streams). Every function is asynchronous; nothing happens at import time.
import { AlefError } from '../core/errors.ts';
import { openReadable, openWritable, type Readable, type Writable } from '../core/stream.ts';
import { call } from '../core/transport.ts';

/** A capture device the runtime reported (`media.camera.list`). */
export interface CameraInfo {
  id: string;
  label: string;
  width: number;
  height: number;
  fps: number;
}

/** One video frame: the stream header plus the `width*height*4` RGBA bytes. */
export interface CameraFrame {
  width: number;
  height: number;
  timestamp: number;
  seq: number;
  dropped: number;
  format: 'rgba';
  data: Uint8Array<ArrayBuffer>;
}

/** A capture device the runtime reported (`media.microphone.list`). */
export interface MicrophoneInfo {
  id: string;
  label: string;
  rate: number;
  frequency: number;
}

/** One PCM chunk: the stream header plus little-endian f32 samples. */
export interface PcmChunk {
  timestamp: number;
  seq: number;
  dropped: number;
  rate: number;
  samples: number;
  format: 'f32le';
  data: Float32Array<ArrayBuffer>;
}

/** What the runtime reports when a camera is closed. */
export interface CameraStats { camera: number; frames: number; dropped: number; }
/** What the runtime reports when a microphone is closed. */
export interface MicrophoneStats { microphone: number; chunks: number; samples: number; dropped: number; }

/** What the runtime reports when a sink is closed. */
export interface AudioSinkStats {
  samples: number;
  checksum: string;
}

/** What the application asks of `camera.open`. */
export interface CameraOpenOptions {
  fps?: number;
  signal?: AbortSignal;
}

/** What the application asks of `microphone.open`. */
export interface MicrophoneOpenOptions {
  frequency?: number;
  rate?: number;
  signal?: AbortSignal;
}

/** What the application asks of `audio.open`. */
export interface AudioOpenOptions {
  signal?: AbortSignal;
}

/** The json header that precedes every binary payload of a media stream. */
type MediaHeader = { seq: number; timestamp: number; dropped: number; byteLength: number };

/**
 * Pairs every binary payload of `readable` with the json header before it: the header is followed by
 * one or more binary chunks whose lengths sum to the header's `byteLength` (a short chunk is normal,
 * never an error; a missing or extra byte, or a new header before completion, is an internal error).
 */
async function* pairs<Header extends MediaHeader>(
  readable: Readable,
): AsyncGenerator<{ header: Header; data: Uint8Array<ArrayBuffer> }> {
  let header: Header | undefined;
  let assembled: Uint8Array<ArrayBuffer> | undefined;
  let received = 0;
  for await (const frame of readable) {
    if (frame.kind === 'json') {
      if (header !== undefined) throw new AlefError('INTERNAL', 'Media stream: a header arrived before the previous payload was complete.');
      const length = (frame.value as { byteLength?: unknown }).byteLength;
      if (typeof length !== 'number' || !Number.isInteger(length) || length < 0) {
        throw new AlefError('INTERNAL', 'Media stream: the header has no byteLength.');
      }
      header = frame.value as Header;
      assembled = new Uint8Array(length);
      received = 0;
      if (length === 0) {
        const done = { header, data: assembled };
        header = undefined;
        assembled = undefined;
        yield done;
      }
    } else if (frame.kind === 'binary') {
      if (header === undefined || assembled === undefined) throw new AlefError('INTERNAL', 'Media stream: a chunk arrived without a header.');
      if (received + frame.data.length > assembled.length) throw new AlefError('INTERNAL', 'Media stream: the payload is longer than the header promised.');
      assembled.set(frame.data, received);
      received += frame.data.length;
      if (received === assembled.length) {
        const done = { header, data: assembled };
        header = undefined;
        assembled = undefined;
        yield done;
      }
    }
  }
  if (header !== undefined) throw new AlefError('INTERNAL', 'Media stream: the stream ended inside a payload.');
}

/** An opened camera: an async iterable of `CameraFrame`s that must be `close()`d. */
export class Camera {
  readonly id: number;
  readonly stream: number;
  #signal: AbortSignal | undefined;
  #readable: Readable | undefined;

  constructor(open: { camera: number; stream: number }, options: { signal?: AbortSignal }) {
    this.id = open.camera;
    this.stream = open.stream;
    this.#signal = options.signal;
  }

  /** Opens the stream on the first iteration, then yields one frame per header+payload pair. */
  async *[Symbol.asyncIterator](): AsyncGenerator<CameraFrame> {
    this.#readable ??= await openReadable(this.stream, { signal: this.#signal });
    for await (const frame of pairs<MediaHeader & Pick<CameraFrame, 'width' | 'height'>>(this.#readable)) {
      yield { ...frame.header, format: 'rgba' as const, data: frame.data };
    }
  }

  /**
   * Closes the stream (if it was opened) and the camera in the runtime; the errors propagate. A
   * second close rejects with `NOT_FOUND` (the runtime removed the device).
   */
  async close(): Promise<CameraStats> {
    await this.#readable?.close();
    return call('media.camera.close', { camera: this.id });
  }
}

/** An opened microphone: an async iterable of `PcmChunk`s that must be `close()`d. */
export class Microphone {
  readonly id: number;
  readonly stream: number;
  #signal: AbortSignal | undefined;
  #readable: Readable | undefined;

  constructor(open: { microphone: number; stream: number }, options: { signal?: AbortSignal }) {
    this.id = open.microphone;
    this.stream = open.stream;
    this.#signal = options.signal;
  }

  /** Opens the stream on the first iteration, then yields one chunk per header+payload pair. */
  async *[Symbol.asyncIterator](): AsyncGenerator<PcmChunk> {
    this.#readable ??= await openReadable(this.stream, { signal: this.#signal });
    for await (const frame of pairs<MediaHeader & Pick<PcmChunk, 'rate' | 'samples'>>(this.#readable)) {
      yield { ...frame.header, format: 'f32le' as const, data: new Float32Array(frame.data.buffer, frame.data.byteOffset, frame.data.byteLength / 4) };
    }
  }

  /**
   * Closes the stream (if it was opened) and the microphone in the runtime; the errors propagate. A
   * second close rejects with `NOT_FOUND` (the runtime removed the device).
   */
  async close(): Promise<MicrophoneStats> {
    await this.#readable?.close();
    return call('media.microphone.close', { microphone: this.id });
  }
}

/**
 * An opened audio sink: raw little-endian f32 bytes written into it are played by the runtime.
 * `close()` ends the stream and reports how many samples the sink received.
 */
export class AudioSink {
  readonly id: number;
  readonly stream: number;
  #signal: AbortSignal | undefined;
  #writable: Writable | undefined;

  constructor(open: { sink: number; stream: number }, options: { signal?: AbortSignal }) {
    this.id = open.sink;
    this.stream = open.stream;
    this.#signal = options.signal;
  }

  /** Appends the raw bytes of `samples` to the stream; the first write opens it. */
  async write(samples: Float32Array<ArrayBuffer> | Uint8Array<ArrayBuffer>): Promise<void> {
    this.#writable ??= await openWritable(this.stream, { signal: this.#signal });
    const bytes = samples instanceof Uint8Array
      ? samples
      : new Uint8Array(samples.buffer, samples.byteOffset, samples.byteLength);
    await this.#writable.write(bytes);
  }

  /** Ends the stream and reports the stats of the sink; the errors propagate. */
  async close(): Promise<AudioSinkStats> {
    await this.#writable?.end();
    return call<{ samples: number; checksum: string }>('audio.close', { sink: this.id });
  }
}

function requireNumber(value: unknown, label: string, low: number, high: number): asserts value is number {
  if (typeof value !== 'number' || !Number.isFinite(value) || value < low || value > high) {
    throw new AlefError('INVALID_ARGUMENT', `${label} must be a finite number between ${low} and ${high}.`);
  }
}

async function openCamera(id: string, options: CameraOpenOptions): Promise<Camera> {
  if (typeof id !== 'string' || id === '') throw new AlefError('INVALID_ARGUMENT', 'camera.open(): the id is missing.');
  if (options.fps !== undefined) requireNumber(options.fps, 'camera.open(): fps', 1, 120);
  const reply = await call<{ camera: number; stream: number }>('media.camera.open', { id, fps: options.fps }, { signal: options.signal });
  return new Camera(reply, options);
}

async function openMicrophone(id: string, options: MicrophoneOpenOptions): Promise<Microphone> {
  if (typeof id !== 'string' || id === '') throw new AlefError('INVALID_ARGUMENT', 'microphone.open(): the id is missing.');
  if (options.frequency !== undefined) requireNumber(options.frequency, 'microphone.open(): frequency', 1, 20000);
  if (options.rate !== undefined) requireNumber(options.rate, 'microphone.open(): rate', 8000, 192000);
  const reply = await call<{ microphone: number; stream: number }>(
    'media.microphone.open',
    { id, frequency: options.frequency, rate: options.rate },
    { signal: options.signal },
  );
  return new Microphone(reply, options);
}

export const camera = {
  /** Lists the cameras the runtime reported. */
  list: async (): Promise<CameraInfo[]> => (await call<{ devices: CameraInfo[] }>('media.camera.list', null)).devices,
  /** Opens the camera `id` (`fps` defaults to 30 in the runtime and is clamped there). */
  open: (id: string, options: CameraOpenOptions = {}): Promise<Camera> => openCamera(id, options),
};

export const microphone = {
  /** Lists the microphones the runtime reported. */
  list: async (): Promise<MicrophoneInfo[]> => (await call<{ devices: MicrophoneInfo[] }>('media.microphone.list', null)).devices,
  /** Opens the microphone `id` (the runtime defaults to 440 Hz at 48000 samples/s). */
  open: (id: string, options: MicrophoneOpenOptions = {}): Promise<Microphone> => openMicrophone(id, options),
};

async function openSink(options: AudioOpenOptions): Promise<AudioSink> {
  const reply = await call<{ sink: number; stream: number }>('audio.open', null, { signal: options.signal });
  return new AudioSink(reply, options);
}

export const audio = {
  /** Opens a sink the page writes raw little-endian f32 bytes into. */
  open: (options: AudioOpenOptions = {}): Promise<AudioSink> => openSink(options),
};
