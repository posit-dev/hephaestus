// Prove the three things the placeholder is supposed to buy.
//
//   1. First paint is immediate. The picture is in the served HTML, so it is
//      on screen after an HTML parse and a PNG decode rather than after a
//      3 MB wasm compile.
//   2. The swap costs one paint. There must be no blank frame between the
//      last placeholder frame and the first live one, and no frame showing
//      both — a fade would be a visible ghost, since the two images agree
//      pixel for pixel.
//   3. Nothing shifts. The picture and the canvas are both absolutely
//      positioned inside a box of known size, so cumulative layout shift is
//      zero. A non-zero figure means the page shape is wrong.
//
// Plus the degradation case: with the renderer skipped, the picture must
// still be on screen, opaque, and the topmost thing at the plot's centre.
//
//   node bench/swap.mjs [--port 8092] [--runs 5]
//
// Drives `bench/swap.html`, which holds nothing but the plot: the harness
// hashes whole composited frames, so a status line that changes when the
// renderer finishes would be indistinguishable from the plot changing.
//
// Expects, from the repository root:
//   cargo run --example document_save --features document-write
//   cargo run --example document_placeholder --features vello-hybrid,document-read,png
//   cp examples/document.hep examples/document.png crates/hephaestus-wasm/www/
//   ./build.sh

import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { createHash } from 'node:crypto';

import { launch, poll, serve } from './cdp.mjs';

const HERE = path.resolve(fileURLToPath(new URL('.', import.meta.url)));

const args = process.argv.slice(2);
const value = (name, fallback) => {
  const i = args.indexOf(`--${name}`);
  return i === -1 ? fallback : args[i + 1];
};
const PORT = Number(value('port', 8092));
const RUNS = Number(value('runs', 5));

const median = (xs) => {
  const s = [...xs].sort((a, b) => a - b);
  return s.length % 2 ? s[(s.length - 1) / 2] : (s[s.length / 2 - 1] + s[s.length / 2]) / 2;
};
const ms = (x) => (x === null || x === undefined ? '—' : `${x.toFixed(1)} ms`);

/**
 * Load a page and report its paint timings, its layout shift, and whether the
 * client got as far as a live frame.
 */
async function measure(url, readyExpr, preamble = '') {
  const session = await launch();
  try {
    await session.call('Runtime.enable');
    await session.call('Page.enable');
    // Registered before navigation, so nothing is missed: a paint entry can
    // land before a script in the document body runs.
    await session.call('Page.addScriptToEvaluateOnNewDocument', {
      source: `
        ${preamble}
        window.__paints = [];
        window.__cls = 0;
        new PerformanceObserver((l) => {
          for (const e of l.getEntries()) window.__paints.push({ name: e.name, at: e.startTime });
        }).observe({ type: 'paint', buffered: true });
        new PerformanceObserver((l) => {
          for (const e of l.getEntries()) if (!e.hadRecentInput) window.__cls += e.value;
        }).observe({ type: 'layout-shift', buffered: true });
        new PerformanceObserver((l) => {
          for (const e of l.getEntries()) {
            window.__lcp = { at: e.startTime, element: e.element?.tagName ?? null };
          }
        }).observe({ type: 'largest-contentful-paint', buffered: true });
      `,
    });
    await session.call('Page.navigate', { url });

    // The caller says what "settled" means, because guessing gets it wrong:
    // `readyState` reaches 'complete' well before `create` resolves, so a
    // predicate that accepts either state reads the overlay mid-boot and sees
    // the placeholder still up.
    const ready = await poll(session.call, readyExpr, { timeoutMs: 60000 });

    const state = JSON.parse(
      await poll(
        session.call,
        `JSON.stringify({
           paints: performance.getEntriesByType('paint').map((e) => ({ name: e.name, at: e.startTime })),
           cls: window.__cls,
           lcp: window.__lcp ?? null,
           overlay: (() => {
             const img = document.querySelector('#frame img');
             if (!img) return null;
             const box = img.getBoundingClientRect();
             const top = document.elementFromPoint(
               box.left + box.width / 2, box.top + box.height / 2,
             );
             return {
               present: true,
               opacity: getComputedStyle(img).opacity,
               naturalWidth: img.naturalWidth,
               topmostAtCentre: top ? top.tagName : null,
             };
           })(),
         })`,
      ),
    );
    const fcp = state.paints.find((p) => p.name === 'first-contentful-paint')?.at ?? null;
    return { ready, fcp, ...state, console: session.consoleLines };
  } finally {
    session.done();
  }
}

/**
 * Record every composited frame of a page load and classify each one.
 *
 * Frames are hashed, so "the same picture twice" is one class rather than two
 * near-identical images. Blankness is judged from the frame's own bytes: the
 * placeholder and the live frame both carry the plot, and a cleared canvas
 * would compress to almost nothing by comparison.
 */
async function screencast(url, preamble = '') {
  const session = await launch();
  const frames = [];
  try {
    await session.call('Runtime.enable');
    await session.call('Page.enable');
    if (preamble) {
      await session.call('Page.addScriptToEvaluateOnNewDocument', { source: preamble });
    }
    session.events('Page.screencastFrame', async (p) => {
      frames.push({ at: p.metadata.timestamp, bytes: Buffer.from(p.data, 'base64') });
      try {
        await session.call('Page.screencastFrameAck', { sessionId: p.sessionId });
      } catch {
        // The cast was stopped between the frame and the ack.
      }
    });
    await session.call('Page.startScreencast', {
      format: 'png',
      everyNthFrame: 1,
      maxWidth: 1000,
      maxHeight: 600,
    });
    const navigatedAt = Date.now() / 1000;
    await session.call('Page.navigate', { url });
    await poll(
      session.call,
      `document.getElementById('frame') && 'ready' in document.getElementById('frame').dataset ? 1 : null`,
    );
    // A few frames past the swap, so the sequence has a tail to check.
    await new Promise((r) => setTimeout(r, 500));
    await session.call('Page.stopScreencast');
    return frames.map((f) => ({
      // Frame timestamps are epoch seconds; navigation-relative milliseconds
      // are what a reader can compare against anything else here.
      at: (f.at - navigatedAt) * 1000,
      size: f.bytes.length,
      hash: createHash('sha1').update(f.bytes).digest('hex').slice(0, 12),
    }));
  } finally {
    session.done();
  }
}

const server = await serve(PORT);
let failures = 0;
const check = (ok, label, detail = '') => {
  console.log(`${ok ? 'ok  ' : 'FAIL'}  ${label}${detail ? `  ${detail}` : ''}`);
  if (!ok) failures += 1;
};

try {
  const page = `http://127.0.0.1:${PORT}/bench/swap.html`;
  const live = page;
  const bare = `${page}?picture=0`;
  const noWasm = `${page}?nowasm=1`;

  // ---- 1. first paint, with and without the renderer running.
  const READY_LIVE = `'ready' in (document.getElementById('frame')?.dataset ?? {}) ? 1 : null`;
  // The ?nowasm= page marks itself instead of creating a view.
  const READY_STATIC = `'static' in (document.getElementById('frame')?.dataset ?? {}) ? 1 : null`;

  const withPlaceholder = [];
  for (let i = 0; i < RUNS; i += 1) withPlaceholder.push(await measure(live, READY_LIVE));
  const staticOnly = await measure(noWasm, READY_STATIC);

  const fcps = withPlaceholder.map((r) => r.fcp).filter((x) => x !== null);
  const lcp = withPlaceholder[0].lcp;
  console.log(`\nfirst contentful paint    ${ms(median(fcps))} (median of ${fcps.length})`);
  console.log(`largest contentful paint  ${ms(lcp?.at)} on <${(lcp?.element ?? '?').toLowerCase()}>`);
  console.log(`cumulative layout shift   ${withPlaceholder.map((r) => r.cls).join(', ')}`);

  check(fcps.length === RUNS, 'every run reported a first contentful paint');
  check(
    withPlaceholder.every((r) => r.cls === 0),
    'no layout shift',
    `cls ${withPlaceholder.map((r) => r.cls).join(', ')}`,
  );
  // Note what LCP does *not* tell us: a canvas draw is not a contentful
  // paint, so with no picture the page has no LCP candidate for the plot at
  // all. That is exactly why the number below comes off the screencast.
  check(
    lcp?.element === 'IMG',
    'the placeholder is the largest contentful paint',
    `got <${(lcp?.element ?? 'none').toLowerCase()}>`,
  );

  // ---- 2. the overlay's two phases.
  const after = withPlaceholder[0].overlay;
  check(after?.present === true, 'the overlay survives the swap (saveOnRightClick is on)');
  check(after?.opacity === '0', 'it is transparent afterwards', `opacity ${after?.opacity}`);
  check(
    after?.topmostAtCentre === 'IMG',
    'it is still the hit-test target, so right-click gets an image menu',
    `topmost <${(after?.topmostAtCentre ?? 'none').toLowerCase()}>`,
  );

  const degraded = staticOnly.overlay;
  check(degraded?.present === true, 'with no renderer the picture is still in the DOM');
  check(degraded?.opacity === '1', 'and still opaque', `opacity ${degraded?.opacity}`);
  check(
    (degraded?.naturalWidth ?? 0) > 0,
    'and actually loaded',
    `naturalWidth ${degraded?.naturalWidth}`,
  );
  check(degraded?.topmostAtCentre === 'IMG', 'and is what the viewer can act on');

  // ---- 3. when do plot pixels reach the screen, and does the swap cost one
  //         paint? Both come off the same recording. A canvas draw is not a
  //         paint entry, so the screencast is the only ground truth.
  // `?picture=0` is the same page with no placeholder, which is what a
  // producer that cannot rasterise would ship. Comparing against it isolates
  // the feature rather than comparing two different pages.
  const casts = { with: await screencast(live), without: await screencast(bare) };

  /** Collapse a recording into runs of identical frames. */
  const runsOf = (cast) => {
    const out = [];
    for (const f of cast) {
      if (!out.length || out[out.length - 1].hash !== f.hash) {
        out.push({ hash: f.hash, size: f.size, at: f.at, count: 1 });
      } else {
        out[out.length - 1].count += 1;
      }
    }
    return out;
  };

  // A cleared canvas and an empty document are nearly uniform, so they encode
  // far smaller than a frame carrying the plot.
  const withRuns = runsOf(casts.with);
  const withoutRuns = runsOf(casts.without);
  // The frame carrying the plot encodes several times larger than an empty
  // document or a cleared canvas, so half the largest frame separates the two
  // cleanly — the observed ratio is around 6x, not a marginal call.
  const biggest = Math.max(...[...withRuns, ...withoutRuns].map((c) => c.size));
  const isBlank = (c) => c.size < biggest / 2;

  const firstPainted = (runs) => runs.findIndex((c) => !isBlank(c));
  const iWith = firstPainted(withRuns);
  const iWithout = firstPainted(withoutRuns);

  console.log('\ntime to plot pixels on screen');
  console.log(`  with the picture     ${ms(withRuns[iWith]?.at)}`);
  console.log(`  without it          ${ms(withoutRuns[iWithout]?.at)}`);

  console.log(`\n${casts.with.length} composited frames in ${withRuns.length} runs of identical pixels:`);
  for (const c of withRuns) {
    console.log(`  ${c.hash}  x${c.count}  ${String(c.size).padStart(6)} bytes  at ${ms(c.at)}`);
  }

  check(
    iWith !== -1 && iWithout !== -1,
    'both recordings reached a painted frame',
  );
  check(
    withRuns[iWith].at < withoutRuns[iWithout].at,
    'the picture puts the plot on screen sooner than the renderer can',
    `${ms(withRuns[iWith]?.at)} against ${ms(withoutRuns[iWithout]?.at)}`,
  );

  const gaps = withRuns.slice(iWith + 1).filter(isBlank);
  check(
    gaps.length === 0,
    'no blank frame between the picture and the plot',
    gaps.length ? `${gaps.length} blank frame run(s) after content` : '',
  );
  check(
    withRuns.length - iWith <= 2,
    'content goes picture then plot, with nothing in between',
    `${withRuns.length - iWith} painted frame(s)`,
  );

  if (withPlaceholder[0].console.length) {
    console.log('\npage console:');
    for (const line of withPlaceholder[0].console) console.log(`  ${line}`);
  }
} finally {
  server.kill();
}

console.log(failures ? `\n${failures} check(s) failed` : '\nall checks passed');
process.exitCode = failures ? 1 : 0;
