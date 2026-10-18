#!/usr/bin/env node
// How often does a streamed reply actually reach the page? Runs the browser
// journey N times against one server and reports, per run, the largest change the
// TRANSCRIPT saw after the reply started. A run that never changes is a run where
// the reply stayed invisible until a resume — the user's bug.
//
//   node scripts/probe-replies.mjs <origin> <folder> [runs]
//
// Sequential, and each Chrome is reaped before the next starts, so N runs cost
// one Chrome's memory, not N.
import { spawn } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const [origin, folder, runsArg] = process.argv.slice(2);
const runs = Number(runsArg || 4);
const chromePath = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const outdir = mkdtempSync(join(tmpdir(), "evo-replies-"));

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function oneRun(n) {
  const profile = mkdtempSync(join(tmpdir(), "evo-r-"));
  const child = spawn(chromePath, [
    "--headless=new", "--remote-debugging-port=0", `--user-data-dir=${profile}`,
    "--window-size=1280,800", "--no-first-run", "--no-default-browser-check",
    "--disable-background-networking", "--enable-unsafe-swiftshader", "about:blank",
  ], { stdio: ["ignore", "pipe", "pipe"], detached: true });
  const kill = () => {
    try { process.kill(-child.pid, "SIGKILL"); } catch { try { child.kill("SIGKILL"); } catch {} }
    rmSync(profile, { recursive: true, force: true });
  };
  try {
    const ep = await new Promise((res, rej) => {
      let log = "";
      const on = (d) => { log += d.toString(); const m = log.match(/ws:\/\/[^\s]+/); if (m) res(m[0]); };
      child.stderr.on("data", on); child.stdout.on("data", on);
      setTimeout(() => rej(new Error("no endpoint")), 30000);
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
    const CLIP = { x: 340, y: 150, width: 790, height: 520, scale: 1 };
    const grab = async () => {
      const r = await send("Page.captureScreenshot", { format: "png", clip: CLIP }, sid);
      return r?.data ?? null;
    };
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
        for (let i = 0; i < pa.length; i += 4)
          if (Math.abs(pa[i]-pb[i])+Math.abs(pa[i+1]-pb[i+1])+Math.abs(pa[i+2]-pb[i+2]) > 24) n++;
        return n;
      })()`;
      const r = await send("Runtime.evaluate", { expression: expr, returnByValue: true, awaitPromise: true }, sid);
      return r?.result?.value ?? -1;
    };

    await sleep(9000);
    await click(920, 297); await sleep(400);
    await feed(folder); await sleep(700);
    await click(920, 329); await sleep(10000);

    const tabs = await (await fetch(`${origin}/api/tabs`)).json();
    const tab = tabs.tabs[tabs.tabs.length - 1].id;
    const before = await grab();
    await feed(`run ${n}: write a 120 word paragraph about rivers.`);
    await sleep(500);
    await q(`(() => { const t = [...document.querySelectorAll('textarea')].pop(); t.focus();
      t.dispatchEvent(new InputEvent('beforeinput', { bubbles: true, inputType: 'insertLineBreak' })); return 'sent'; })()`);

    let maxDelta = 0, serverMax = 0, prev = before;
    for (let i = 0; i < 16; i++) {
      await sleep(1500);
      let chars = 0;
      try {
        const d = await (await fetch(`${origin}/api/tabs/${tab}/snapshot?topics=session&items=80`)).json();
        const a = d.topics.session.items.filter((x) => x.kind === "assistant");
        chars = a.length ? (a[a.length - 1].text || "").length : 0;
      } catch { /* busy */ }
      serverMax = Math.max(serverMax, chars);
      const now = await grab();
      const d2 = prev ? await diff(prev, now) : 0;
      if (d2 > maxDelta) maxDelta = d2;
      prev = now;
    }
    kill();
    const drew = maxDelta > 1000;
    console.log(`run ${n}: server_chars=${String(serverMax).padStart(4)}  max_transcript_delta=${String(maxDelta).padStart(6)}  ${drew ? "DREW" : "*** FROZEN ***"}`);
    return drew;
  } catch (e) {
    kill();
    console.log(`run ${n}: error ${e.message}`);
    return null;
  }
}

const results = [];
for (let i = 1; i <= runs; i++) results.push(await oneRun(i));
const drew = results.filter((r) => r === true).length;
const frozen = results.filter((r) => r === false).length;
console.log(`\n${drew}/${results.length} drew the reply, ${frozen} frozen`);
rmSync(outdir, { recursive: true, force: true });
process.exit(0);
