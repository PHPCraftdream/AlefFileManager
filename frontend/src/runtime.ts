let capability: string | undefined;

/** In-process Servo transport; no server address or OS endpoint. */
export async function invoke<T>(command: string, arguments_: unknown = null, signal?: AbortSignal): Promise<T> {
  if (capability === undefined) {
    capability = new URLSearchParams(window.location.hash.slice(1)).get('capability') ?? '';
  }
  if (!capability) throw new Error('Start the native application with npm run dev or npm start.');
  const response = await fetch('native://invoke/', {
    method: 'POST',
    headers: { Authorization: `Bearer ${capability}`, 'Content-Type': 'application/json' },
    body: JSON.stringify({ command, arguments: arguments_ }),
    signal,
  });
  const body = await response.json() as T | { error: string };
  if (!response.ok) {
    throw new Error(typeof body === 'object' && body !== null && 'error' in body
      ? body.error : `Native command failed (${response.status})`);
  }
  return body as T;
}
