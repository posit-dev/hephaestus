// A Chrome DevTools Protocol session over Node's own WebSocket, so the harness
// needs no dependencies. Shared by every script in this directory.

import { spawn } from 'node:child_process';
import path from 'node:path';

/** Where Chrome lives, overridable for a machine that puts it elsewhere. */
export const CHROME =
  process.env.HEPHAESTUS_CHROME ??
  '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';

/**
 * Launch a headless Chrome with a fresh profile and attach to one tab.
 *
 * `ratio` pins the device pixel ratio: Chrome's headless default is 1, and a
 * ratio the page did not expect changes the canvas backing store, which is
 * one variable too many for a measurement.
 *
 * Returns `{ call, events, consoleLines, done }`. `call` issues a CDP command
 * on the tab's session; `events(method)` registers a listener; `done` tears
 * both the socket and the browser down.
 */
export async function launch({ ratio = 1, profile, args = [] } = {}) {
  const dir =
    profile ?? path.join(process.env.TMPDIR ?? '/tmp', `hephaestus-cdp-${process.pid}`);

  const child = spawn(
    CHROME,
    [
      '--headless=new',
      '--remote-debugging-port=0',
      `--user-data-dir=${dir}`,
      '--no-first-run',
      '--no-default-browser-check',
      '--disable-extensions',
      `--force-device-scale-factor=${ratio}`,
      '--window-size=1400,900',
      ...args,
      'about:blank',
    ],
    { stdio: ['ignore', 'ignore', 'pipe'] },
  );

  const endpoint = await new Promise((resolve, reject) => {
    let buf = '';
    const timer = setTimeout(
      () => reject(new Error(`Chrome did not report a devtools endpoint\n${buf}`)),
      30000,
    );
    child.stderr.on('data', (d) => {
      buf += d;
      const m = buf.match(/ws:\/\/[^\s]+/);
      if (m) {
        clearTimeout(timer);
        resolve(m[0]);
      }
    });
    child.on('exit', (code) => {
      clearTimeout(timer);
      reject(new Error(`Chrome exited with ${code}\n${buf}`));
    });
  });

  const ws = new WebSocket(endpoint);
  await new Promise((resolve, reject) => {
    ws.addEventListener('open', resolve, { once: true });
    ws.addEventListener('error', reject, { once: true });
  });

  let next = 1;
  const pending = new Map();
  const listeners = new Map();
  const consoleLines = [];

  ws.addEventListener('message', (ev) => {
    const msg = JSON.parse(ev.data);
    if (msg.id && pending.has(msg.id)) {
      const { resolve, reject } = pending.get(msg.id);
      pending.delete(msg.id);
      if (msg.error) reject(new Error(JSON.stringify(msg.error)));
      else resolve(msg.result);
      return;
    }
    if (msg.method === 'Runtime.consoleAPICalled') {
      consoleLines.push(msg.params.args.map((a) => a.value ?? a.description).join(' '));
    } else if (msg.method === 'Runtime.exceptionThrown') {
      consoleLines.push(`EXCEPTION ${msg.params.exceptionDetails.text}`);
    }
    for (const fn of listeners.get(msg.method) ?? []) fn(msg.params);
  });

  const send = (method, params = {}, sessionId) =>
    new Promise((resolve, reject) => {
      const id = next++;
      pending.set(id, { resolve, reject });
      ws.send(JSON.stringify({ id, method, params, sessionId }));
    });

  const { targetId } = await send('Target.createTarget', { url: 'about:blank' });
  const { sessionId } = await send('Target.attachToTarget', { targetId, flatten: true });

  return {
    call: (method, params) => send(method, params, sessionId),
    events(method, fn) {
      if (!listeners.has(method)) listeners.set(method, []);
      listeners.get(method).push(fn);
    },
    consoleLines,
    done() {
      try {
        ws.close();
      } catch {
        // Already gone; the kill below is what matters.
      }
      child.kill();
    },
  };
}

/**
 * Serve the crate directory on `port`, resolving once it is listening.
 *
 * Returns the child, for the caller to kill.
 */
export async function serve(port, extra = []) {
  const server = spawn(
    process.execPath,
    [path.join(path.dirname(new URL(import.meta.url).pathname), 'server.mjs'), '--port', String(port), ...extra],
    { stdio: ['ignore', 'pipe', 'inherit'] },
  );
  await new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('server did not start')), 10000);
    server.stdout.on('data', (d) => {
      if (String(d).includes('serving')) {
        clearTimeout(timer);
        resolve();
      }
    });
  });
  return server;
}

/** Poll an expression in the page until it stops being `null`. */
export async function poll(call, expression, { timeoutMs = 60000, everyMs = 50 } = {}) {
  const started = Date.now();
  for (;;) {
    const { result } = await call('Runtime.evaluate', { expression, returnByValue: true });
    if (result.value !== null && result.value !== undefined) return result.value;
    if (Date.now() - started > timeoutMs) throw new Error(`timed out waiting for: ${expression}`);
    await new Promise((r) => setTimeout(r, everyMs));
  }
}
