// SPDX-License-Identifier: MIT OR Apache-2.0
import { api, guard, rejection, suite, verdict } from './harness.js';

async function bounded(promise) {
  let timer;
  try {
    return await Promise.race([promise, new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error('command did not finish within 15 s')), 15000);
    })]);
  } finally { clearTimeout(timer); }
}

async function cleanup(child) {
  const error = await bounded(rejection(child.kill()));
  if (error && error.code !== 'NOT_FOUND') throw error;
}

function expect(actual, expected) {
  if (JSON.stringify(actual) !== JSON.stringify(expected)) {
    throw new Error(`got ${JSON.stringify(actual)}, expected ${JSON.stringify(expected)}`);
  }
}

async function main() {
  const { check, failed } = suite();
  const literal = 'literal space ; & $(not-a-shell)';
  await check('cli-run-declared-params-and-body', async () => {
    const result = await bounded(api.cli.run('echo', { prefix: literal }, { input: 'hello', timeout: 10000 }));
    expect(result, { code: 0, signal: null, stdout: `${literal}:hello`, stderr: 'warn' });
  });
  await check('cli-start-declared-params-and-streams', async () => {
    const child = await bounded(api.cli.start('echo', { prefix: literal }));
    try {
      const output = new Response(child.stdout).text();
      const errors = new Response(child.stderr).text();
      const send = (async () => {
        const writer = child.stdin.getWriter();
        try {
          await writer.write(new TextEncoder().encode('stream'));
          await writer.close();
        } finally { writer.releaseLock(); }
      })();
      const [stdout, stderr, ended] = await bounded(Promise.all([output, errors, child.wait(), send]));
      expect(stdout, `${literal}:stream`);
      expect(stderr, 'warn');
      expect(ended, { code: 0, signal: null });
    } finally {
      await cleanup(child);
    }
  });
  await check('cli-sidecar-declared-and-direct-launch', async () => {
    const result = await bounded(api.cli.run('bundled', { value: literal }, { timeout: 10000 }));
    expect(result.stdout, literal);
    expect(result.code, 0);
    const direct = await bounded(api.cli.exec('sidecar:node -v', { timeout: 10000 }));
    if (direct.code !== 0 || !direct.stdout.startsWith('v')) throw new Error('bundled node did not run');
  });
  await check('cli-declared-command-rights-are-separate', async () => {
    expect((await bounded(rejection(api.cli.run('undeclared'))))?.code, 'PERMISSION_DENIED');
    expect((await bounded(rejection(api.cli.exec('node -v'))))?.code, 'PERMISSION_DENIED');
  });
  await check('cli-declared-substituted-times-out', async () => {
    expect((await bounded(rejection(api.cli.run('substituted', undefined, { timeout: 200 }))))?.code, 'TIMEOUT');
  });
  await verdict(failed());
}

guard(main);
