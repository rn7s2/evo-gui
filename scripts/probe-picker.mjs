#!/usr/bin/env node
// Prove the browser's folder picker: press "Select folder…" on the empty tab and
// show that the page asked the serving machine for a directory listing.
//
// `scripts/probe.mjs` is the general probe; this is the one thing it cannot show.
// A gpui page draws to a canvas, so there is no DOM node to click and no
// accessibility tree to read — the control is at a coordinate. What *can* be
// watched is the traffic: the picker's whole work is `GET /api/fs`, and a page
// that never asks for one has not opened a picker.
//
//   node scripts/probe-picker.mjs <origin> [--x N] [--y N] [--wait MS]

import { spawn } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const argv = process.argv.slice(2);
const arg = (name, fallback) => {
  const i = argv.indexOf(`--${name}`);
  return i >= 0 && argv[i + 1] ? argv[i + 1] : fallback;
};
const origin = argv[0];
if (!origin || origin.startsWith("--")) {
  console.error("usage: probe-picker.mjs <origin> [--x N] [--y N] [--wait MS]");
  process.exit(2);
}
const chromePath = arg("chrome", "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome");
// The walk asks its question with `window.prompt`, and headless Chrome suppresses
// dialogs entirely (no user to answer one). `--answer N` stubs the page's `prompt`
// before it loads, so the walk can be driven to its end: the number is what a person
// would have typed.
const answer = arg("answer", null);
// What to answer a dialog the click opened: `accept` is "yes", and anything else
// leaves it dismissed — which is what a person clicking Cancel does.
const acceptDialog = arg("accept-dialog", null);
// A second click, after the first has been given time to land: opening a session
// and then closing *its* tab is two gestures, and each probe run starts from a fresh
// page with one empty tab.
const thenX = arg("then-x", null);
const thenY = arg("then-y", null);
const x = Number(arg("x", 640));
const y = Number(arg("y", 300));
const waitMs = Number(arg("wait", 9000));

const profile = mkdtempSync(join(tmpdir(), "evo-picker-"));
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
    "--enable-unsafe-swiftshader",
    "about:blank",
  ],
  { stdio: ["ignore", "pipe", "pipe"], detached: true },
);
const cleanup = () => {
  try { process.kill(-child.pid, "SIGKILL"); } catch { try { child.kill("SIGKILL"); } catch {} }
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

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const ws = new WebSocket(endpoint);
let id = 0;
const pending = new Map();
const send = (method, params, sessionId) =>
  new Promise((r) => {
    const i = ++id;
    pending.set(i, r);
    ws.send(JSON.stringify({ id: i, method, params, sessionId }));
  });
// Every request the page makes, and every dialog it opens: the two things a
// picker shows up as.
const requests = [];
const dialogs = [];
ws.onmessage = (e) => {
  const m = JSON.parse(e.data);
  if (m.id && pending.has(m.id)) {
    pending.get(m.id)(m.result);
    pending.delete(m.id);
    return;
  }
  if (m.method === "Network.requestWillBeSent") {
    requests.push({ method: m.params.request.method, url: m.params.request.url });
  }
  if (m.method === "Page.javascriptDialogOpening") {
    dialogs.push(m.params.message);
    if (acceptDialog !== null) {
      send("Page.handleJavaScriptDialog", { accept: acceptDialog === "yes" }, sid);
    }
  }
};
await new Promise((r) => (ws.onopen = r));

const target = await send("Target.createTarget", { url: `${origin}/` });
const { sessionId: sid } = await send("Target.attachToTarget", { targetId: target.targetId, flatten: true });
await send("Runtime.enable", {}, sid);
await send("Network.enable", {}, sid);
await send("Page.enable", {}, sid);
if (answer !== null) {
  await send(
    "Page.addScriptToEvaluateOnNewDocument",
    { source: `window.prompt = () => ${JSON.stringify(answer)};` },
    sid,
  );
  // The page is already up; reload it so the stub is in place before it runs.
  await send("Page.reload", {}, sid);
  await sleep(6000);
}
// A prompt would otherwise block the page forever with nobody to answer it.
await send("Page.setInterceptFileChooserDialog", { enabled: true }, sid);
ws.addEventListener("message", (e) => {
  const m = JSON.parse(e.data);
  if (m.method === "Page.fileChooserOpened") dialogs.push(`<file chooser: ${m.params.mode}>`);
});

await sleep(waitMs);

const before = requests.filter((r) => r.url.includes("/api/fs")).length;
const beforeDialogs = dialogs.length;

const clickAt = async (cx, cy) => {
  for (const type of ["mousePressed", "mouseReleased"]) {
    await send("Input.dispatchMouseEvent", { type, x: cx, y: cy, button: "left", clickCount: 1 }, sid);
    await sleep(120);
  }
};
await clickAt(x, y);
if (thenX !== null) {
  // The first click's work has to finish first: a tab that is not on the strip yet
  // has no ✕ to click.
  await sleep(Number(arg("then-wait", 16000)));
  await clickAt(Number(thenX), Number(thenY ?? y));
}
// The picker's first step is one `GET /api/fs`; the page's own prompt follows it,
// and with `--answer` the walk goes on from there and launches.
await sleep(2500);
if (acceptDialog !== null) {
  await sleep(6000);
}
if (answer !== null) {
  // The walk goes on from the first listing: with `0` the answer is "this folder",
  // so the launch follows it — which is a `POST /api/tabs`, and that is what the
  // network log below is read for.
  await sleep(9000);
}

const of = (path) => requests.filter((r) => r.url.includes(path));
const after = of("/api/fs");
const report = {
  origin,
  answer,
  clicked: { x, y },
  apiFsRequestsBeforeTheClick: before,
  apiFsRequestsAfterTheClick: after.length,
  firstListing: after[0] ? after[0].url : null,
  launches: of("/api/tabs").map((r) => `${r.method} ${r.url}`),
  deletes: requests
    .filter((r) => r.method === "DELETE" && r.url.includes("/api/tabs"))
    .map((r) => r.url),
  dialogs: dialogs,
  dialogsOpenedByTheClick: dialogs.length - beforeDialogs,
  allApiRequests: of("/api/").map((r) => `${r.method} ${r.url}`).slice(0, 20),
  everyRequest: requests.map((r) => `${r.method} ${r.url}`).slice(-14),
};
console.log(JSON.stringify(report, null, 2));
cleanup();
process.exit(0);
