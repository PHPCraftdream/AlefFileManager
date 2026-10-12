// Single instance and before-quit. The runner starts two processes of this application: the first
// (`--role=first`) takes the endpoint and waits to hear of the second (`--role=second one.txt two.txt`),
// which learns that it is not the first and quits. The first one then vetoes a quit and allows the next.
import { api, guard, report, suite, until, verdict } from './harness.js';

const expectEqual = (what, actual, expected) => {
  if (JSON.stringify(actual) !== JSON.stringify(expected)) {
    throw new Error(`${what}: ${JSON.stringify(actual)}, expected ${JSON.stringify(expected)}`);
  }
};

async function first(check, failed, cwd) {
  const heard = [];
  await check('the-first-instance-is-told-so', async () => {
    expectEqual('first answer', await api.app.requestSingleInstance(), true);
    expectEqual('asking again', await api.app.requestSingleInstance(), true);
  });
  await api.app.on('second-instance', info => heard.push(info));
  await report(`path appCache ${await api.path.appCache()}`);
  await report('instance first-ready');
  await check('a-later-instance-is-announced-with-its-arguments-and-directory', async () => {
    await until(() => heard.length > 0, 90000, 'the second instance');
    const [info] = heard;
    expectEqual('role', info.args.parsed.role, 'second');
    expectEqual('files', info.args.positional, ['one.txt', 'two.txt']);
    expectEqual('raw', info.args.raw, ['--role=second', 'one.txt', 'two.txt']);
    expectEqual('working directory', info.cwd, cwd);
    await new Promise(resolve => setTimeout(resolve, 500));
    expectEqual('announcements', heard.length, 1);
  });
  await check('before-quit-can-be-vetoed-and-then-allowed', async () => {
    let asked = 0;
    await api.app.on('before-quit', event => {
      asked += 1;
      if (asked <= 2) event.preventDefault();
    });
    await api.app.quit(9);
    expectEqual('questions after the first quit', asked, 1);
    await api.app.relaunch(); // asked as well: a vetoed relaunch starts nothing and quits nothing
    expectEqual('questions after the relaunch', asked, 2);
    expectEqual('the application still answers', (await api.app.info()).id, 'org.alef.e2e.modules.instance');
  });
  await verdict(failed());
  await api.app.quit(9); // the third question is allowed: the process ends with code 9
}

async function second(check, failed) {
  await check('a-later-instance-is-told-so', async () => {
    expectEqual('answer', await api.app.requestSingleInstance(), false);
    expectEqual('asking again', await api.app.requestSingleInstance(), false);
  });
  await verdict(failed());
  await api.app.quit(0);
}

async function main() {
  const { cwd } = await (await fetch('targets.json')).json();
  const { check, failed } = suite();
  const { role } = (await api.app.args()).parsed;
  if (role === 'first') return first(check, failed, cwd);
  if (role === 'second') return second(check, failed);
  throw new Error(`unknown role ${role}`);
}

guard(main);
