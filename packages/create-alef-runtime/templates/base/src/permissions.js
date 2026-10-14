import { fs, http, path } from '@alef-runtime/api';

function describe(error) {
  if (error?.code === 'PERMISSION_DENIED') return 'Permission denied (intentional with the closed starter manifest).';
  if (error?.code === 'TIMEOUT' || error?.code === 'CANCELLED' || error?.name === 'AbortError') return 'Request timed out or was cancelled.';
  return `Unavailable: ${error?.code ?? error?.message ?? String(error)}`;
}

// The app-data directory and example.com are stand-ins, not permission grants.
// The fully closed manifest intentionally denies reads and HTTP; narrow its scopes
// only for your real use case. Timeouts/aborts are ordinary failures, not detection.
export async function permissionExample(deps = { fs, http, path }) {
  const messages = [];
  try {
    const directory = await deps.path.appData();
    const entries = await deps.fs.readDir(directory);
    messages.push(entries.length ? `App-data entries: ${entries.map(entry => entry.name).join(', ')}` : 'App-data directory is empty.');
  } catch (error) {
    messages.push(`Files: ${describe(error)}`);
  }

  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 2000);
  try {
    const response = await deps.http.request('https://example.com', { timeout: 1500, signal: controller.signal });
    messages.push(`HTTP status: ${response.status}`);
    // Read the status only, then cancel the body so no stream stays open.
    await response.body?.cancel().catch(() => undefined);
  } catch (error) {
    messages.push(`HTTP: ${describe(error)}`);
  } finally {
    clearTimeout(timer);
  }
  return messages;
}
