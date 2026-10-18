#!/usr/bin/env node
// Does the TRANSCRIPT grow while the reply streams? Diffs only the transcript
// region (clipped, so the composer's blinking caret cannot register), and prints
// the server's own assistant-text length alongside, sample by sample.
//
//   node scripts/probe-transcript.mjs <origin> <folder> <text> <outdir>
//
// Kills its own Chrome; never touches the user's.
import { spawn } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const [origin, folder, text, outdir] = process.argv.slice(2);
const chromePath = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
mkdirSync(outdir, { recursive: true });

const profile = mkdtempSync(join(tmpdir(), "evo-tr-"));
const child = spawn(chromePath, [
  "--headless=new", "--remote-debugging-port=0", `--user-data-dir=${profile}`,
  "--window-size=1280,800", "--no-first-run", "--no-default-browser-check",
  "--disable-background-networking", "--enable-unsafe-swiftshader", "about:blank",
], { stdio: ["ignore", "pipe", "pipe"], detached: true });
const cleanup = () => {
  try { process.kill(-child.pid, "SIGKILL"); } catch { try { child.kill("SIGKILL"); } catch {} }
  rmSync(profile, { recursive: true, force: true });
};
for (const sig of ["SIGINT", "SIGTERM", "uncaughtException"]) {
  process.on(sig, (e) => { cleanup(); if (e) console.error(e); process.exit(1); });
}

const ep = await new Promise((res, rej) => {
  let log = "";
  const on = (d) => { log += d.toString(); const m = log.match(/ws:\/\/[^\s]+/); if (m) res(m[0]); };
  child.stderr.on("data", on); child.stdout.on("data", on);
  setTimeout(() => rej(new Error("no devtools endpoint")), 30000);
});
const ws = new WebSocket(ep);
let id = 0; const pend = new Map();
const send = (m, p, s) => new Promise((r) => {
  const i = ++id; pend.set(i, r); ws.send(JSON.stringify({ id: i, method: m, params: p, sessionId: s }));
});
ws.onmessage = (e) => {
  const m = JSON.parse(e.data);
  if (m.id && pend.has(m.id)) { pend.get(m.id)(m.result); pend.delete(m.id); }
};
await new Promise((r) => (ws.onopen = r));
const t = await send("Target.createTarget", { url: `${origin}/` });
const s = await send("Target.attachToTarget", { targetId: t.targetId, flatten: true });
const sid = s.sessionId;
await send("Runtime.enable", {}, sid);

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const q = async (expression) => {
  const r = await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true }, sid);
  return r?.result?.value ?? null;
};
const click = async (x, y) => {
  for (const type of ["mousePressed", "mouseReleased"]) {
    await send("Input.dispatchMouseEvent", { type, x, y, button: "left", clickCount: 1 }, sid);
    await sleep(80);
  }
};
const feed = async (v) => q(`(() => { const t = [...document.querySelectorAll('textarea')].pop(); if (!t) return 'none';
  t.focus(); t.value = ${JSON.stringify(v)}; t.dispatchEvent(new Event('input', { bubbles: true })); return t.value.length; })()`);

// The transcript, clipped so the composer (and its caret) is out of frame.
const CLIP = { x: 340, y: 150, width: 790, height: 520, scale: 1 };
const grab = async () => {
  const r = await send("Page.captureScreenshot", { format: "png", clip: CLIP }, sid);
  return r?.data ?? null;
};
// Compare two captures in the page, where a canvas can read them.
const diff = async (a, b) => {
  const expr = `(async () => {
    const load = (src) => new Promise((r) => { const i = new Image(); i.onload = () => r(i); i.src = "data:image/png;base64," + src; });
    const [A, B] = await Promise.all([load(${JSON.stringify(a)}), load(${JSON.stringify(b)})]);
    const w = A.width, h = A.height;
    const c = document.createElement('canvas'); c.width = w; c.height = h;
    const x = c.getContext('2d');
    x.drawImage(A, 0, 0); const pa = x.getImageData(0, 0, w, h).data;
    x.clearRect(0, 0, w, h); x.drawImage(B, 0, 0); const pb = x.getImageData(0, 0, w, h).data;
    let n = 0;
    for (let i = 0; i < pa.length; i += 4) {
      if (Math.abs(pa[i] - pb[i]) + Math.abs(pa[i+1] - pb[i+1]) + Math.abs(pa[i+2] - pb[i+2]) > 24) n++;
    }
    return n;
  })()`;
  const r = await send("Runtime.evaluate", { expression: expr, returnByValue: true, awaitPromise: true }, sid);
  return r?.result?.value ?? -1;
};

await sleep(10000);
await click(920, 297); await sleep(400);
await feed(folder); await sleep(700);
await click(920, 329); await sleep(11000);

const tabs = await (await fetch(`${origin}/api/tabs`)).json();
const tab = tabs.tabs[tabs.tabs.length - 1].id;
console.log("tab", tab.slice(0, 8));

const before = await grab();
await feed(text); await sleep(500);
await q(`(() => { const t = [...document.querySelectorAll('textarea')].pop(); t.focus();
  t.dispatchEvent(new InputEvent('beforeinput', { bubbles: true, inputType: 'insertLineBreak' })); return 'sent'; })()`);

const t0 = Date.now();
let prev = before;
for (let i = 0; i < 20; i++) {
  await sleep(1500);
  const el = ((Date.now() - t0) / 1000).toFixed(1);
  let chars = -1, preview = "";
  try {
    const d = await (await fetch(`${origin}/api/tabs/${tab}/snapshot?topics=session&items=80`)).json();
    const its = d.topics.session.items;
    const a = its.filter((x) => x.kind === "assistant");
    chars = a.length ? (a[a.length - 1].text || "").length : 0;
    preview = a.length ? (a[a.length - 1].text || "").replace(/\s+/g, " ").slice(0, 26) : "";
  } catch { /* busy */ }
  const now = await grab();
  const changed = prev ? await diff(prev, now) : -1;
  prev = now;
  console.log(`t=${el}s  server_chars=${String(chars).padStart(4)}  transcript_delta_px=${String(changed).padStart(6)}  ${preview}`);
  if (i === 19) writeFileSync(join(outdir, "final.png"), Buffer.from(now, "base64"));
}
cleanup();
process.exit(0);
