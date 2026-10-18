#!/usr/bin/env node
// Probe the browser build of evo-gui through CDP: load the page, capture the
// console, optionally drive the UI, and report what the page actually saw.
//
// This exists because the browser build's failures are invisible to `cargo
// check` and to API-level tests: a wasm trap, a stalled stream or a missing
// font only shows up in a real page. Every previous investigation re-wrote this
// script by hand; this is the one copy.
//
//   node scripts/probe.mjs <origin> [--chrome PATH] [--folder PATH] [--prompt TEXT]
//                                  [--wait MS] [--screenshot FILE] [--json FILE]
//                                  [--dump-log]
//
// It prints a JSON report: the console lines, the exceptions, whether the canvas
// mounted, cross-origin isolation, the DOM's textareas, and — when --folder is
// given — whether a session opened and whether the session holds --prompt.
import { spawn } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const argv = process.argv.slice(2);
const arg = (name, fallback) => {
  const i = argv.indexOf(`--${name}`);
  return i >= 0 && argv[i + 1] ? argv[i + 1] : fallback;
};
const has = (name) => argv.includes(`--${name}`);

const origin = argv[0];
if (!origin || origin.startsWith("--")) {
  console.error("usage: probe.mjs <origin> [--chrome PATH] [--folder PATH] [--prompt TEXT]");
  process.exit(2);
}
const chromePath =
  arg("chrome", "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome");
const folder = arg("folder", null);
const prompt = arg("prompt", null);
const waitMs = Number(arg("wait", 10000));
const screenshot = arg("screenshot", null);

const profile = mkdtempSync(join(tmpdir(), "evo-probe-"));
const child = spawn(
  chromePath,
  [
    "--headless=new",
    "--remote-debugging-port=0",
    `--user-data-dir=${profile}`,
    "--window-size=1280,800",
    "--no-first-run",
    "--no-default-browser-check",
    "--disable-background-networking",
    "--disable-component-update",
    "--disable-sync",
    "--mute-audio",
    // Headless has no GPU; gpui prefers WebGPU and falls back to WebGL2.
    "--enable-unsafe-swiftshader",
    "about:blank",
  ],
  { stdio: ["ignore", "pipe", "pipe"], detached: true },
);

const cleanup = () => {
  try {
    process.kill(-child.pid, "SIGKILL");
  } catch {
    try {
      child.kill("SIGKILL");
    } catch {}
  }
  rmSync(profile, { recursive: true, force: true });
};

const endpoint = await new Promise((resolve, reject) => {
  let log = "";
  const on = (d) => {
    log += d.toString();
    const m = log.match(/ws:\/\/[^\s]+/);
    if (m) resolve(m[0]);
  };
  child.stderr.on("data", on);
  child.stdout.on("data", on);
  setTimeout(() => reject(new Error(`no devtools endpoint:\n${log}`)), 30000);
});

const ws = new WebSocket(endpoint);
let id = 0;
const pending = new Map();
const send = (method, params, sessionId) =>
  new Promise((r) => {
    const i = ++id;
    pending.set(i, r);
    ws.send(JSON.stringify({ id: i, method, params, sessionId }));
  });
ws.onmessage = (e) => {
  const m = JSON.parse(e.data);
  if (m.id && pending.has(m.id)) {
    pending.get(m.id)(m.result);
    pending.delete(m.id);
  }
};
await new Promise((r) => (ws.onopen = r));

const target = await send("Target.createTarget", { url: `${origin}/` });
const attached = await send("Target.attachToTarget", {
  targetId: target.targetId,
  flatten: true,
});
const sid = attached.sessionId;
await send("Runtime.enable", {}, sid);

const console_ = [];
ws.addEventListener("message", (e) => {
  const m = JSON.parse(e.data);
  if (m.method === "Runtime.consoleAPICalled") {
    console_.push({
      level: m.params.type,
      text: m.params.args
        .map((a) => String(a.value ?? a.description ?? ""))
        .join(" "),
    });
  }
  if (m.method === "Runtime.exceptionThrown") {
    console_.push({
      level: "exception",
      text: m.params.exceptionDetails.text +
        " " + (m.params.exceptionDetails.exception?.description || ""),
    });
  }
});

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const evalIn = async (expression) => {
  const r = await send(
    "Runtime.evaluate",
    { expression, returnByValue: true, awaitPromise: true },
    sid,
  );
  if (r && r.exceptionDetails) return `EXC ${r.exceptionDetails.text}`;
  return r && r.result ? r.result.value : null;
};

const click = async (x, y) => {
  for (const type of ["mousePressed", "mouseReleased"]) {
    await send("Input.dispatchMouseEvent", { type, x, y, button: "left", clickCount: 1 }, sid);
    await sleep(80);
  }
};

// gpui-web takes text through a hidden IME-mirror textarea: set its value and
// fire `input`. `Input.dispatchKeyEvent`/`insertText` do NOT reach the app.
const typeInto = async (text) =>
  evalIn(`(() => {
    const t = [...document.querySelectorAll('textarea')].pop();
    if (!t) return 'no mirror';
    t.focus();
    t.value = ${JSON.stringify(text)};
    t.dispatchEvent(new Event('input', { bubbles: true }));
    return t.value.length;
  })()`);

// gpui maps `beforeinput` insertLineBreak to a synthetic Enter.
const pressEnter = async () =>
  evalIn(`(() => {
    const t = [...document.querySelectorAll('textarea')].pop();
    if (!t) return 'no mirror';
    t.focus();
    t.dispatchEvent(new InputEvent('beforeinput', { bubbles: true, inputType: 'insertLineBreak' }));
    return 'ok';
  })()`);

await sleep(waitMs);

const report = { origin, isolated: null, canvas: null, booted: null, textareas: null, steps: [] };
report.isolated = await evalIn(`String(globalThis.crossOriginIsolated)`);
report.canvas = await evalIn(
  `(() => { const c = document.querySelector('body > canvas'); return c ? c.clientWidth + 'x' + c.clientHeight : 'none'; })()`,
);
report.booted = await evalIn(`!document.getElementById('evo-boot')`);
report.textareas = await evalIn(`document.querySelectorAll('textarea').length`);

if (folder) {
  // The empty tab's folder field and its button, at the coordinates that work in
  // a 1280x800 headless window (the DOM cannot say: gpui draws to a canvas and
  // gpui-web exposes no accessibility tree).
  await click(920, 297);
  await sleep(400);
  await typeInto(folder);
  await sleep(700);
  await click(920, 329);
  await sleep(9000);

  const tabs = await evalIn(`fetch('/api/tabs').then(r => r.text())`);
  let tab = null;
  try {
    tab = (JSON.parse(tabs).tabs || [])[0] || null;
  } catch {}
  report.steps.push(["a session opened", !!tab, tab ? `${tab.id.slice(0, 8)} ${tab.state}` : String(tabs).slice(0, 120)]);

  if (prompt && tab) {
    // The composer is focused on attach; feed it and press Enter.
    await typeInto(prompt);
    await sleep(800);
    await pressEnter();
    await sleep(9000);
    const snap = await evalIn(`fetch('/api/tabs/${tab.id}/snapshot?topics=session').then(r => r.text())`);
    const held = typeof snap === "string" && snap.includes(prompt);
    report.steps.push(["the session holds the prompt", held, held ? "a user item carries it" : String(snap).slice(0, 140)]);
  }
}

if (screenshot) {
  const shot = await send("Page.captureScreenshot", { format: "png" }, sid);
  writeFileSync(screenshot, Buffer.from(shot.data, "base64"));
  report.screenshot = screenshot;
}

const interesting = console_.filter(
  (c) => !/Wasm assets loading/.test(c.text) && !/Failed to fetch icon/.test(c.text),
);
report.console = has("dump-log") ? console_ : interesting.slice(0, 40);
report.consoleCount = console_.length;
report.panics = console_.filter((c) => /panicked at/.test(c.text)).length;

console.log(JSON.stringify(report, null, 2));
cleanup();
process.exit(0);
