// SPDX-License-Identifier: MIT OR Apache-2.0
//! A tiny stand-in for the user's `rush` CLI, driven by the ai_rush integration tests.
//! Invocation: `node rush_emulator.mjs <mode> <rush args...>` — argv[2] is the mode, the rest
//! are the arguments the provider passes. All recording goes under the value of `--data-dir`:
//! argv.json (every full argv), env.json (every `run`: the ALEF_* keys and the env size),
//! sessions.log (`sessions cancel|delete <id>` lines), run.pid (every `run`, written first).
import * as fs from "node:fs";
import * as path from "node:path";
import { spawn } from "node:child_process";

const mode = process.argv[2] ?? "normal";
const args = process.argv.slice(3);
const subcommand = args[0] ?? "";
const flag = (name) => {
  const at = args.indexOf(name);
  return at >= 0 ? args[at + 1] : undefined;
};
const has = (name) => args.includes(name);

const dataDir = flag("--data-dir");

/** Writes the whole buffer to a descriptor, waiting out a full non-blocking pipe. */
function writeAll(fd, buffer) {
  let at = 0;
  while (at < buffer.length) {
    try {
      at += fs.writeSync(fd, buffer, at, buffer.length - at);
    } catch (error) {
      if (error.code !== "EAGAIN") throw error;
      Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 5);
    }
  }
}

function main() {
  function record(name, text, append) {
    if (dataDir === undefined) return;
    fs.mkdirSync(dataDir, { recursive: true });
    const file = path.join(dataDir, name);
    if (append) fs.appendFileSync(file, text);
    else fs.writeFileSync(file, text);
  }

  // argv.json: append the full argv of every invocation (a JSON array push).
  if (dataDir !== undefined) {
    fs.mkdirSync(dataDir, { recursive: true });
    const argvFile = path.join(dataDir, "argv.json");
    let list = [];
    try {
      list = JSON.parse(fs.readFileSync(argvFile, "utf8"));
    } catch {
      list = [];
    }
    list.push(process.argv);
    fs.writeFileSync(argvFile, JSON.stringify(list));
  }

  if (subcommand === "sessions") {
    // sessions.log: "<word> <id>" per invocation.
    record("sessions.log", `${args[1] ?? ""} ${args[2] ?? ""}\n`, true);
  }

  if (subcommand === "models") {
    if (mode === "models-slow") {
      // Slow probe: five seconds before a normal models answer.
      Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 5000);
    }
    const effective =
      mode === "wrapper"
        ? { smart: { provider: "openai-codex" }, fast: { provider: "openai-codex" } }
        : { smart: { provider: "anthropic" }, fast: { provider: "openai" } };
    if (mode === "reviewer") effective.reviewer = { provider: "anthropic" };
    process.stdout.write(`${JSON.stringify({ effective })}\n`);
    process.exit(0);
  }

  if (subcommand === "run") {
    // run.pid goes out before anything else, so the test can find the child early.
    record("run.pid", `${process.pid}`, false);
    record(
      "env.json",
      `${JSON.stringify({
        alef: Object.keys(process.env)
          .filter((key) => key.startsWith("ALEF_"))
          .sort(),
        count: Object.keys(process.env).length,
      })}\n`,
      false,
    );
    let prompt = "";
    if (mode !== "hang" && mode !== "no-stdin") {
      try {
        prompt = fs.readFileSync(0, "utf8");
      } catch {
        prompt = "";
      }
    }
    const json = has("--json");
    if (mode === "fail") {
      process.stderr.write("RUSH-STDERR-SECRET sentinel overflow /home/nobody/.rush/db\n");
      process.exit(3);
    }
    if (mode === "garbage") {
      process.stdout.write(Buffer.from([0xff, 0xfe]));
      process.stdout.write("not json at all\n");
      process.exit(0);
    }
    if (mode === "hang" || mode === "no-stdin") {
      if (mode === "hang") {
        const descendant = spawn(process.execPath, ["-e", "setInterval(() => {}, 1000)"], {
          stdio: "ignore",
        });
        record("descendant.pid", `${descendant.pid}`, false);
      }
      // Never exit: the event loop stays alive on this timer until the tree is killed.
      setInterval(() => {}, 1000);
      return;
    }
    if (mode === "eof-hang") {
      // One short line, then stdout closes (EOF) while the process stays alive.
      process.stdout.write("eof-hang\n");
      try {
        fs.closeSync(1);
      } catch {
        // Already closed.
      }
      setInterval(() => {}, 1000);
      return;
    }
    if (mode === "oversize") {
      // Synchronous writes: exit(0) must not drop what a pipe has not taken yet.
      const chunk = Buffer.alloc(64 * 1024, "x");
      for (let i = 0; i < 5; i += 1) writeAll(1, chunk);
      writeAll(1, Buffer.from(`${"x".repeat(12 * 1024)}\n`));
      process.exit(0);
    }
    if (mode === "fast-lines") {
      for (let i = 0; i < 64; i += 1) {
        fs.writeSync(1, `line-${i}-${"x".repeat(8192 - 8)}\n`);
        fs.writeFileSync(path.join(dataDir, "progress"), String(i + 1));
      }
      process.exit(0);
    }

  if (mode === "stderr-flood") {
    // Deliberately cross the provider's 4 MiB stderr cap while continuing to drain stdout.
    const chunk = `${"e".repeat(64 * 1024 - 1)}\n`;
    for (let i = 0; i < 129; i += 1) fs.writeSync(2, chunk);
  }
  if (mode === "huge-line") {
    // One 2 MiB line without a newline; the bridge must cap storage and reject it.
    for (let i = 0; i < 32; i += 1) fs.writeSync(1, "x".repeat(64 * 1024));
  }
  if (json) {
      process.stdout.write(
        `${JSON.stringify({
          final_text: prompt,
          usage: { input_tokens: 11, output_tokens: 7 },
          exit_reason: "completed",
        })}\n`,
      );
    } else {
      for (const line of ["hello", "world", "done"]) {
        process.stdout.write(`${line}\n`);
      }
    }
    process.exit(0);
  }

  // Anything else (no arguments at all): answer like the models probe.
  process.stdout.write(
    `${JSON.stringify({ effective: { smart: { provider: "anthropic" }, fast: { provider: "openai" } } })}\n`,
  );
  process.exit(0);
}

main();
