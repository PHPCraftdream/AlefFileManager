// The `cli` module: `exec` (code, streams, shell, timeout, env, rights) and `spawn` (streams both
// ways, kill of the whole tree). The scenario runs in two consent modes: everything allowed, and the
// right `cli.exec:node` substituted (the call hangs and times out, nothing starts).
import { api, guard, rejection, same, suite, until, verdict, pattern } from './harness.js';

const STREAM_SIZE = 2 * 1024 * 1024;

async function cleanup(child) {
  try { await child.kill(); }
  catch (error) { if (error?.code !== 'NOT_FOUND') throw error; } // wait already consumed the process
}

/** The process behind `pid` is gone: a node child probes it (kill with 0 only asks). */
async function gone(pid, what) {
  await until(async () => {
    try {
      const probe = await api.cli.exec(`node -e "try { process.kill(${pid}, 0) } catch { process.exit(0) } process.exit(1)"`);
      return probe.code === 0;
    } catch { return false; }
  }, 15000, what);
}

async function main() {
  const { mode, work, outside } = await (await fetch('targets.json')).json();
  const { check, failed } = suite();
  const slash = text => text.replaceAll('\\', '/');
  const pidFile = `${slash(work)}/grandchild.pid`;

  if (mode === 'allowed') {
    const expect = async (what, actual, expected) => {
      if (actual !== expected) throw new Error(`${what}: ${JSON.stringify(actual)}, expected ${JSON.stringify(expected)}`);
    };
    await check('cli-exec-runs-a-program-and-reports-its-code-and-output', async () => {
      const result = await api.cli.exec('node -e "process.stdout.write(String(42+1)); process.exitCode = 3"');
      await expect('code', result.code, 3);
      await expect('stdout', result.stdout, '43');
      await expect('signal', result.signal, null);
      await expect('stderr', result.stderr, '');
    });
    await check('cli-exec-feeds-stdin-and-captures-stderr', async () => {
      const result = await api.cli.exec(
        'node -e "let d=\'\'; process.stdin.on(\'data\', c => d += c); process.stdin.on(\'end\', () => { process.stderr.write(\'e\'); process.stdout.write(\'got:\' + d) })"',
        { input: 'hello' },
      );
      await expect('stdout', result.stdout, 'got:hello');
      await expect('stderr', result.stderr, 'e');
    });
    await check('cli-exec-through-a-shell', async () => {
      const result = await api.cli.exec('echo cli-shell-ok', { shell: true });
      if (!result.stdout.includes('cli-shell-ok')) throw new Error(`stdout ${JSON.stringify(result.stdout)}, code ${result.code}`);
    });
    await check('cli-exec-times-out-and-kills-the-tree', async () => {
      const file = `${slash(work)}/hung.pid`;
      // Poll while exec is running; startup is not assumed to fit an 800 ms deadline.
      // The runtime timeout still includes startup: even this larger budget can expire under load.
      const outcome = rejection(api.cli.exec(
        `node -e "require('fs').writeFileSync(process.argv[1], String(process.pid)); setInterval(() => {}, 1e5)" "${file}"`,
        { timeout: 15000 },
      ));
      let pid;
      const ready = until(async () => {
        try {
          pid = Number(await api.fs.readText(file));
          return pid > 0;
        } catch { return false; }
      }, 14000, 'the timeout child to write its pid before the deadline');
      const [error] = await Promise.all([outcome, ready]);
      if (error?.code !== 'TIMEOUT') throw new Error(`timeout: ${error?.code ?? 'it succeeded'}`);
      await gone(pid, 'the timed out child to die');
    });
    await check('cli-spawn-pipes-streams-both-ways', async () => {
      const child = await api.cli.spawn('node', ['-e', 'process.stdin.pipe(process.stdout)']);
      try {
        if (!(typeof child.pid === 'number' && child.pid > 0)) throw new Error(`pid ${child.pid}`);
        if (!(child.stdin instanceof WritableStream) || !(child.stdout instanceof ReadableStream) || !(child.stderr instanceof ReadableStream)) {
          throw new Error('the streams are missing');
        }
        const sent = pattern(STREAM_SIZE);
        const chunks = [];
        const reader = child.stdout.getReader();
        const writer = child.stdin.getWriter();
        const receive = (async () => {
          try {
            for (;;) {
              const { done, value } = await reader.read();
              if (done) break;
              chunks.push(value);
            }
          } finally { reader.releaseLock(); }
        })();
        const send = (async () => {
          try {
            for (let at = 0; at < sent.length; at += 65536) await writer.write(sent.subarray(at, at + 65536));
            await writer.close();
          } finally { writer.releaseLock(); }
        })();
        await Promise.all([receive, send]);
        const got = new Uint8Array(chunks.reduce((total, chunk) => total + chunk.length, 0));
        let at = 0;
        for (const chunk of chunks) { got.set(chunk, at); at += chunk.length; }
        if (!same(got, sent)) throw new Error(`the echo returned ${got.length} bytes instead of ${sent.length}`);
        const ended = await child.wait();
        if (ended.code !== 0) throw new Error(`wait: ${JSON.stringify(ended)}`);
        // stderr was opened as a pipe too: nothing went there and the pipe ends with the process.
        const errReader = child.stderr.getReader();
        try {
          const err = await errReader.read();
          if (!err.done && err.value.length > 0) throw new Error(`stderr is not empty: ${err.value.length} bytes`);
        } finally { errReader.releaseLock(); }
      } finally {
        await cleanup(child);
      }
    });
    await check('cli-kill-takes-down-the-grandchild', async () => {
      const grandchild = [
        "const { spawn } = require('child_process');",
        "const c = spawn(process.execPath, ['-e', \"require('fs').writeFileSync(process.env.GRAND_PID, String(process.pid)); setInterval(() => {}, 1e5)\"],",
        '{ stdio: \'ignore\', env: { ...process.env, GRAND_PID: process.env.GRAND_PID } });',
        'setInterval(() => {}, 1e5)',
      ].join(' ');
      const child = await api.cli.spawn('node', ['-e', grandchild], { env: { GRAND_PID: pidFile } });
      try {
        await until(async () => {
          try {
            const text = await api.fs.readText(pidFile);
            return Number(text) > 0;
          } catch { return false; }
        }, 15000, 'the grandchild to write its pid');
        const grand = Number(await api.fs.readText(pidFile));
        await child.kill();
        await gone(grand, 'the grandchild to die');
        const ended = await child.wait();
        if (ended.code === null && ended.signal === null) throw new Error(`wait after kill: ${JSON.stringify(ended)}`);
      } finally {
        await cleanup(child);
      }
    });
    await check('cli-rights-are-held', async () => {
      const missing = await rejection(api.cli.exec('definitely-not-a-program-xyz'));
      // Consent *=allow only grants declared rights; it does not broaden the manifest.
      if (missing?.code !== 'PERMISSION_DENIED') throw new Error(`unknown program: ${missing?.code ?? 'it succeeded'}`);
      const operators = await rejection(api.cli.exec('echo a && echo b', { shell: true }));
      if (operators?.code !== 'INVALID_ARGUMENT') throw new Error(`operators: ${operators?.code ?? 'it succeeded'}`);
      const env = await rejection(api.cli.exec('node -v', { env: { PATH: 'nowhere' } }));
      if (env?.code !== 'INVALID_ARGUMENT') throw new Error(`env PATH: ${env?.code ?? 'it succeeded'}`);
      const cwd = await rejection(api.cli.exec('node -v', { cwd: outside }));
      if (cwd?.code !== 'PERMISSION_DENIED') throw new Error(`cwd outside: ${cwd?.code ?? 'it succeeded'}`);
    });
    await check('cli-exec-resolution-ignores-the-page-env', async () => {
      const version = await api.cli.exec('node -v', { env: { ALEF_CLI_E2E: 'x' } });
      if (version.code !== 0 || !version.stdout.startsWith('v')) {
        throw new Error(`node -v: code ${version.code}, stdout ${JSON.stringify(version.stdout)}`);
      }
      const sent = await api.cli.exec('node -e "process.stdout.write(process.env.ALEF_CLI_E2E)"', { env: { ALEF_CLI_E2E: 'sent' } });
      await expect('env passed through', sent.stdout, 'sent');
    });
  } else {
    await check('cli-exec-substituted-hangs-and-times-out-quickly', async () => {
      const error = await rejection(api.cli.exec('node -v', { timeout: 500 }));
      if (error?.code !== 'TIMEOUT') throw new Error(`substitute: ${error?.code ?? 'it succeeded'}`);
    });
  }

  await verdict(failed());
}

guard(main);
