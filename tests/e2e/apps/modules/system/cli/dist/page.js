// The `cli` module: `exec` (code, streams, shell, timeout, env, rights) and `spawn` (streams both
// ways, kill of the whole tree). The scenario runs in two consent modes: everything allowed, and the
// right `cli.exec:node` substituted (the call hangs and times out, nothing starts).
import { api, guard, rejection, same, suite, until, verdict, pattern } from './harness.js';

const STREAM_SIZE = 2 * 1024 * 1024;

async function bounded(promise, what, ms = 15000) {
  let timer;
  try {
    return await Promise.race([promise, new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(`timeout waiting for ${what}`)), ms);
    })]);
  } finally { clearTimeout(timer); }
}

function terminalOutput(terminal) {
  const reader = terminal.readable.getReader();
  const decoder = new TextDecoder();
  let text = '';
  const done = (async () => {
    try {
      for (;;) {
        const next = await reader.read();
        if (next.done) break;
        text += decoder.decode(next.value, { stream: true });
      }
      text += decoder.decode();
    } finally { reader.releaseLock(); }
  })();
  // Observe errors immediately, including when readiness fails before awaiting the pump.
  done.catch(() => {});
  return { done, text: () => text };
}

async function terminalCleanup(terminal, writer, output, wait) {
  try {
    await bounded(cleanup(terminal), 'terminal kill');
    await bounded(wait ?? terminal.wait(), 'terminal cleanup wait').catch(error => {
      if (error?.code !== 'NOT_FOUND') throw error;
    });
  } finally {
    try {
      await bounded(writer.abort(), 'terminal input abort').catch(error => {
        if (error?.code !== 'NOT_FOUND') throw error;
      });
    }
    finally {
      writer.releaseLock();
      await bounded(output.done, 'terminal output end');
    }
  }
}

async function cleanup(child) {
  try { await bounded(child.kill(), 'child cleanup kill'); }
  catch (error) { if (error?.code !== 'NOT_FOUND') throw error; } // wait already consumed the process
}

/** The process behind `pid` is gone: a node child probes it (kill with 0 only asks). */
async function gone(pid, what) {
  await until(async () => {
    try {
      const probe = await api.cli.exec(`node -e "try { process.kill(${pid}, 0) } catch { process.exit(0) } process.exit(1)"`, { timeout: 2000 });
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
        await bounded(Promise.all([receive, send]), 'spawn stream transfer', 30000);
        const got = new Uint8Array(chunks.reduce((total, chunk) => total + chunk.length, 0));
        let at = 0;
        for (const chunk of chunks) { got.set(chunk, at); at += chunk.length; }
        if (!same(got, sent)) throw new Error(`the echo returned ${got.length} bytes instead of ${sent.length}`);
        const ended = await bounded(child.wait(), 'child exit');
        if (ended.code !== 0) throw new Error(`wait: ${JSON.stringify(ended)}`);
        // stderr was opened as a pipe too: nothing went there and the pipe ends with the process.
        const errReader = child.stderr.getReader();
        try {
          const err = await bounded(errReader.read(), 'child stderr EOF');
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
        const ended = await bounded(child.wait(), 'child exit');
        if (ended.code === null && ended.signal === null) throw new Error(`wait after kill: ${JSON.stringify(ended)}`);
      } finally {
        await cleanup(child);
      }
    });
    await check('cli-pty-is-interactive-and-resizes', async () => {
      const program = [
        "process.stdin.setRawMode(true); let pending = '';",
        "console.log('PTY_READY:' + JSON.stringify({ tty: [process.stdin.isTTY, process.stdout.isTTY, process.stderr.isTTY], cols: process.stdout.columns, rows: process.stdout.rows, cwd: process.cwd(), env: process.env.ALEF_PTY_E2E }));",
        "process.stdin.on('data', bytes => { pending += bytes.toString(); let at; while ((at = pending.indexOf('\\r')) >= 0) { const line = pending.slice(0, at); pending = pending.slice(at + 1);",
        "if (line === 'size') console.log('PTY_SIZE:' + process.stdout.columns + ':' + process.stdout.rows);",
        "else if (line === 'exit') process.exit(7);",
        "else { console.log('PTY_ECHO:' + line); console.error('PTY_STDERR:' + line); } } });",
      ].join(' ');
      const terminal = await api.cli.pty('node', ['-e', program], { cols: 80, rows: 24, cwd: work, env: { ALEF_PTY_E2E: 'sent' } });
      const writer = terminal.writable.getWriter();
      const output = terminalOutput(terminal);
      let wait;
      const send = line => bounded(writer.write(new TextEncoder().encode(`${line}\r`)), 'terminal input');
      try {
        if (!(terminal.pid > 0)) throw new Error(`terminal pid ${terminal.pid}`);
        // ConPTY wraps a long JSON record at the configured terminal width.
        const readiness = () => /PTY_READY:({.*?})/.exec(output.text().replaceAll('\r', '').replaceAll('\n', ''));
        await until(() => readiness() !== null, 15000, 'terminal readiness').catch(error => { throw new Error(`${error.message}; output=${JSON.stringify(output.text())}`); });
        const ready = JSON.parse(readiness()[1]);
        if (!ready.tty.every(value => value === true) || ready.cols !== 80 || ready.rows !== 24 || ready.env !== 'sent' || slash(ready.cwd).toLowerCase() !== slash(work).toLowerCase()) {
          throw new Error(`terminal readiness ${JSON.stringify(ready)}`);
        }
        await send('hello');
        await until(() => output.text().includes('PTY_ECHO:hello') && output.text().includes('PTY_STDERR:hello'), 15000, 'merged terminal output');
        await bounded(terminal.resize(101, 37), 'terminal resize');
        await until(async () => {
          await send('size');
          return output.text().includes('PTY_SIZE:101:37');
        }, 15000, 'resized terminal dimensions');
        wait = terminal.wait();
        wait.catch(() => {});
        await send('exit');
        const ended = await bounded(wait, 'terminal exit');
        if (ended.code !== 7) throw new Error(`terminal wait ${JSON.stringify(ended)}`);
        await bounded(output.done, 'terminal EOF');
      } finally { await terminalCleanup(terminal, writer, output, wait); }
    });
    await check('cli-pty-kill-takes-down-the-grandchild', async () => {
      const beat = `${slash(work)}/pty-grandchild.beat`;
      const stop = `${slash(work)}/pty-grandchild.stop`;
      const grandchild = [
        "const fs = require('fs'); let pulse = 0;",
        "const deadline = Date.now() + 45000;",
        "setInterval(() => { if (Date.now() >= deadline || fs.existsSync(process.env.GRAND_STOP)) process.exit(0);",
        "fs.writeFileSync(process.env.GRAND_BEAT, String(++pulse)); }, 100);",
      ].join(' ');
      const program = [
        "const { spawn } = require('child_process');",
        `spawn(process.execPath, ['-e', ${JSON.stringify(grandchild)}], { detached: process.platform === 'win32', stdio: 'ignore', env: process.env });`,
        "console.log('PTY_TREE_READY'); setInterval(() => {}, 1e5);",
      ].join(' ');
      const terminal = await api.cli.pty('node', ['-e', program], { cols: 80, rows: 24, cwd: work, env: { GRAND_BEAT: beat, GRAND_STOP: stop } });
      const writer = terminal.writable.getWriter();
      const output = terminalOutput(terminal);
      let wait;
      const pulse = () => bounded(api.fs.readText(beat), 'grandchild beat read', 2000);
      const stopped = async () => {
        let previous = await pulse();
        let since = Date.now();
        await until(async () => {
          const next = await pulse();
          if (next !== previous) { previous = next; since = Date.now(); }
          return Date.now() - since >= 1500;
        }, 15000, 'detached grandchild pulse to stop');
      };
      try {
        await until(() => output.text().includes('PTY_TREE_READY'), 15000, 'terminal tree readiness');
        let previous;
        await until(async () => {
          try {
            const next = await pulse();
            const advancing = Number(next) > Number(previous);
            previous = next;
            return advancing;
          } catch { return false; }
        }, 15000, 'detached grandchild beat to advance');
        wait = terminal.wait();
        wait.catch(() => {});
        await bounded(terminal.kill('SIGKILL'), 'terminal tree kill');
        const ended = await bounded(wait, 'killed terminal wait');
        if (ended.code === null && ended.signal === null) throw new Error(`terminal wait ${JSON.stringify(ended)}`);
        await stopped();
      } finally {
        // A stop sentinel and a self-deadline also contain a detached survivor when tree kill fails.
        try {
          await api.cli.exec('node -e "require(\'fs\').writeFileSync(process.env.GRAND_STOP, \'stop\')"', { cwd: work, env: { GRAND_STOP: stop }, timeout: 5000 });
          await stopped().catch(error => { if (error?.code !== 'NOT_FOUND') throw error; });
        } finally {
          try { await terminalCleanup(terminal, writer, output, wait); }
          finally {
            await api.cli.exec('node -e "for (const p of [process.env.GRAND_BEAT, process.env.GRAND_STOP]) require(\'fs\').rmSync(p, { force: true })"', { cwd: work, env: { GRAND_BEAT: beat, GRAND_STOP: stop }, timeout: 5000 });
          }
        }
      }
    });
    await check('cli-pty-validates-dimensions-and-rights', async () => {
      const options = { cols: 80, rows: 24, cwd: work, env: {} };
      for (const dimensions of [{ cols: 0 }, { rows: 1001 }, { cols: 1.5 }]) {
        const error = await rejection(api.cli.pty('node', [], { ...options, ...dimensions }));
        if (error?.code !== 'INVALID_ARGUMENT') throw new Error(`dimensions: ${error?.code ?? 'it succeeded'}`);
      }
      for (const [program, extra, code] of [
        ['definitely-not-a-program-xyz', {}, 'PERMISSION_DENIED'],
        ['node', { cwd: outside }, 'PERMISSION_DENIED'],
        ['node', { env: { PATH: 'nowhere' } }, 'INVALID_ARGUMENT'],
      ]) {
        let unexpected;
        try {
          const error = await rejection(api.cli.pty(program, [], { ...options, ...extra }).then(child => { unexpected = child; }));
          if (error?.code !== code) throw new Error(`terminal rights: ${error?.code ?? 'it succeeded'}, expected ${code}`);
        } finally {
          if (unexpected) {
            await bounded(cleanup(unexpected), 'unexpected terminal kill');
            await bounded(unexpected.wait(), 'unexpected terminal wait');
            await bounded(unexpected.writable.abort(), 'unexpected terminal input abort');
            await bounded(unexpected.readable.cancel(), 'unexpected terminal output cancel');
          }
        }
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
    await check('cli-pty-substituted-stays-pending-without-starting-and-aborts', async () => {
      const marker = `${slash(work)}/pty-substitute.marker`;
      const absent = async () => {
        const probe = new AbortController();
        try {
          await bounded(api.fs.stat(marker, { signal: probe.signal }), 'substituted terminal marker probe', 2000);
        } catch (error) {
          if (error?.code === 'NOT_FOUND') return;
          throw error;
        } finally { probe.abort(); }
        throw new Error('substituted terminal started a process: marker exists');
      };
      await absent();
      const controller = new AbortController();
      let settled = false;
      const outcome = api.cli.pty('node', ['-e', "require('fs').writeFileSync(process.argv[1], 'started'); setTimeout(() => process.exit(0), 5000)", marker], {
        cols: 80, rows: 24, cwd: work, signal: controller.signal,
      }).then(child => {
        settled = true;
        return { child };
      }, error => {
        settled = true;
        return { error };
      });
      try {
        // PTY substitution defaults to a 30 s backend timeout; observe pending, then abort first.
        const started = performance.now();
        await bounded(until(async () => {
          if (settled) throw new Error('substituted terminal settled before cancellation');
          await absent();
          if (settled) throw new Error('substituted terminal settled during observation');
          return performance.now() - started >= 750;
        }, 5000, 'substituted terminal pending observation'), 'substituted terminal observation', 6000);
        controller.abort();
        const result = await bounded(outcome, 'substituted terminal cancellation', 5000);
        if (result.error?.name !== 'AbortError') throw new Error(`terminal cancellation: ${result.error?.name ?? 'it returned a process'}`);
        await absent();
      } finally {
        controller.abort();
        const result = await bounded(outcome, 'substituted terminal final cancellation', 5000);
        if (result.child) {
          const ignoreGone = promise => promise.catch(error => { if (error?.code !== 'NOT_FOUND') throw error; });
          try {
            await bounded(cleanup(result.child), 'unexpected substituted terminal kill', 5000);
          } finally {
            try {
              await bounded(ignoreGone(result.child.wait()), 'unexpected substituted terminal wait', 5000);
            } finally {
              try {
                await bounded(ignoreGone(result.child.writable.abort()), 'unexpected substituted terminal writable abort', 5000);
              } finally {
                await bounded(ignoreGone(result.child.readable.cancel()), 'unexpected substituted terminal readable cancel', 5000);
              }
            }
          }
        }
      }
    });
    await check('cli-exec-substituted-hangs-and-times-out-quickly', async () => {
      const error = await rejection(api.cli.exec('node -v', { timeout: 500 }));
      if (error?.code !== 'TIMEOUT') throw new Error(`substitute: ${error?.code ?? 'it succeeded'}`);
    });
  }

  await verdict(failed());
}

guard(main);
