#!/usr/bin/env node
// Watch what the page actually ASKS FOR, and what the engine tells the UI.
//
// The UI's stream is the thing under suspicion: a session can open and take a
// prompt while the page never requests `/stream` at all, which makes the badge
// say "reconnecting..." for ever and makes a sent turn's reply invisible until a
// resume. `scripts/probe.mjs` reports the *result*; this reports the *traffic*.
//
//   node scripts/probe-net.mjs <origin> [--folder PATH] [--wait MS] [--console]
//
// It prints every page-originated request the CDP Network domain sees, the
// console lines, and whether `/stream` was among them.
import { spawn } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const argv = process.argv.slice(2);
const arg = (n, d) => {
  const i = argv.indexOf(`--${n}`);
  return i >= 0 && argv[i + 1] ? argv[i + 1] : d;
};
const has = (n) => argv.includes(`--${n}`);
const origin = argv[0];
if (!origin || origin.startsWith("--")) {
  console.error("usage: probe-net.mjs <origin> [--folder PATH] [--wait MS] [--console]");
  process.exit(2);
}
const chromePath = arg("chrome", "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome");
const folder = arg("folder", null);
const waitMs = Number(arg("wait", 9000));

const profile = mkdtempSync(join(tmpdir(), "evo-net-"));
const child = spawn(chromePath, [
  "--headless=new", "--remote-debugging-port=0", `--user-data-dir=${profile}`,
  "--window-size=1280,800", "--no-first-run", "--no-default-browser-check",
  "--disable-background-networking", "--disable-component-update", "--disable-sync",
  "--mute-audio", "--enable-unsafe-swiftshader", "about:blank",
], { stdio: ["ignore", "pipe", "pipe"], detached: true });

const cleanup = () => {
  try { process.kill(-child.pid, "SIGKILL"); } catch { try { child.kill("SIGKILL"); } catch {} }
  rmSync(profile, { recursive: true, force: true });
};

const endpoint = await new Promise((res, rej) => {
  let log = "";
  const on = (d) => { log += d.toString(); const m = log.match(/ws:\/\/[^\s]+/); if (m) res(m[0]); };
  child.stderr.on("data", on); child.stdout.on("data", on);
  setTimeout(() => rej(new Error("no devtools endpoint")), 30000);
});

const ws = new WebSocket(endpoint);
let id = 0; const pending = new Map();
const send = (m, p, s) => new Promise((r) => {
  const i = ++id; pending.set(i, r); ws.send(JSON.stringify({ id: i, method: m, params: p, sessionId: s }));
});
ws.onmessage = (e) => {
  const m = JSON.parse(e.data);
  if (m.id && pending.has(m.id)) { pending.get(m.id)(m.result); pending.delete(m.id); }
};
await new Promise((r) => (ws.onopen = r));

const target = await send("Target.createTarget", { url: `${origin}/` });
const att = await send("Target.attachToTarget", { targetId: target.targetId, flatten: true });
const sid = att.sessionId;
await send("Runtime.enable", {}, sid);
await send("Network.enable", {}, sid);

const requests = [];
const console_ = [];
ws.addEventListener("message", (e) => {
  const m = JSON.parse(e.data);
  if (m.method === "Network.requestWillBeSent") {
    const r = m.params.request;
    requests.push({ method: r.method, url: r.url, t: m.params.timestamp });
  }
  if (m.method === "Runtime.consoleAPICalled") {
    console_.push({
      level: m.params.type,
      text: m.params.args.map((a) => String(a.value ?? a.description ?? "")).join(" "),
    });
  }
});

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const evalIn = async (expression) => {
  const r = await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true }, sid);
  return r && r.result ? r.result.value : null;
};
const click = async (x, y) => {
  for (const type of ["mousePressed", "mouseReleased"]) {
    await send("Input.dispatchMouseEvent", { type, x, y, button: "left", clickCount: 1 }, sid);
    await sleep(80);
  }
};
const typeInto = async (text) =>
  evalIn(`(() => { const t = [...document.querySelectorAll('textarea')].pop(); if (!t) return 'no mirror';
    t.focus(); t.value = ${JSON.stringify(text)};
    t.dispatchEvent(new Event('input', { bubbles: true })); return t.value.length; })()`);

await sleep(waitMs);
if (folder) {
  await click(920, 297); await sleep(400);
  await typeInto(folder); await sleep(700);
  await click(920, 329); await sleep(9000);
}

const page = requests.filter((r) => !/favicon|\.png$|\.svg$|\.js$|\.wasm$|\.css$/.test(r.url));
console.log("=== page-originated requests ===");
for (const r of page) console.log(`  ${r.method} ${r.url.replace(origin, "")}`);
console.log("=== /stream requested? ===", page.some((r) => r.url.includes("/stream")) ? "YES" : "NO");
if (has("console")) {
  console.log("=== console ===");
  for (const c of console_) console.log(`  [${c.level}] ${c.text.slice(0, 200)}`);
} else {
  const interesting = console_.filter((c) => !/Wasm assets loading|Failed to fetch icon/.test(c.text));
  console.log("=== console (filtered) ===");
  for (const c of interesting.slice(0, 25)) console.log(`  [${c.level}] ${c.text.slice(0, 200)}`);
}
cleanup();
process.exit(0);
