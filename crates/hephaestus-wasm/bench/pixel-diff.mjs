// Compare the frame the wasm client draws against the picture a native build
// rasterises from the same document.
//
// This is the measurement the placeholder story rests on. A host that shows a
// natively-rendered PNG and swaps in the live frame is betting that the two
// agree; if they do not, the swap is a visible glitch rather than a hidden
// one. Antialiasing may differ — the two rasterisers share their CPU strip
// generation but composite through different shaders, and the wasm build runs
// scalar where a native arm64 build runs Neon. A *region* differing means
// something else is wrong: the size, the dpi, the theme or the fonts.
//
//   node bench/pixel-diff.mjs [--w 900] [--h 420] [--ratio 1] [--port 8091]
//
// Expects, from the repository root:
//   cargo run --example document_save --features document-write
//   cargo run --example document_placeholder --features vello-hybrid,document-read,png
//   cp examples/document.hep crates/hephaestus-wasm/www/
//   ./build.sh

import { readFile, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

import { launch, poll, serve } from './cdp.mjs';
import { decodePng, encodePng } from './png.mjs';

const HERE = path.resolve(fileURLToPath(new URL('.', import.meta.url)));
const CRATE = path.resolve(HERE, '..');
const REPO = path.resolve(CRATE, '../..');

const args = process.argv.slice(2);
const value = (name, fallback) => {
  const i = args.indexOf(`--${name}`);
  return i === -1 ? fallback : args[i + 1];
};

const W = Number(value('w', 900));
const H = Number(value('h', 420));
const RATIO = Number(value('ratio', 1));
const PORT = Number(value('port', 8091));

// ---------------------------------------------------------------------- diff

/** Compare two RGBA buffers of the same shape. */
function diff(a, b, width, height) {
  const marks = Buffer.alloc(width * height * 4, 0);
  let differing = 0;
  let overOne = 0;
  let maxDelta = 0;
  const histogram = new Map();
  let minX = width;
  let minY = height;
  let maxX = -1;
  let maxY = -1;

  for (let i = 0; i < a.length; i += 4) {
    let d = 0;
    for (let c = 0; c < 4; c += 1) d = Math.max(d, Math.abs(a[i + c] - b[i + c]));
    if (d === 0) {
      // Differences read better against a dimmed copy of the reference.
      marks[i] = 255 - (255 - a[i]) / 6;
      marks[i + 1] = 255 - (255 - a[i + 1]) / 6;
      marks[i + 2] = 255 - (255 - a[i + 2]) / 6;
      marks[i + 3] = 255;
      continue;
    }
    differing += 1;
    if (d > 1) overOne += 1;
    maxDelta = Math.max(maxDelta, d);
    histogram.set(d, (histogram.get(d) ?? 0) + 1);
    const px = (i / 4) % width;
    const py = Math.floor(i / 4 / width);
    minX = Math.min(minX, px);
    maxX = Math.max(maxX, px);
    minY = Math.min(minY, py);
    maxY = Math.max(maxY, py);
    marks[i] = 255;
    marks[i + 1] = 0;
    marks[i + 2] = 0;
    marks[i + 3] = 255;
  }

  return {
    total: width * height,
    differing,
    overOne,
    maxDelta,
    histogram: [...histogram.entries()].sort((x, y) => x[0] - y[0]),
    bbox: maxX < 0 ? null : { minX, minY, maxX, maxY },
    marks,
  };
}

/**
 * How isolated the differing pixels are.
 *
 * An antialiased fringe is one or two pixels wide, so a differing pixel there
 * has few differing neighbours. A shifted label or a re-wrapped line differs
 * in a solid blob, where almost every differing pixel is fully surrounded.
 */
function clumpiness(marks, width, height) {
  const isDiff = (x, y) =>
    x >= 0 && y >= 0 && x < width && y < height && marks[(y * width + x) * 4 + 1] === 0;
  let counted = 0;
  let surrounded = 0;
  for (let y = 0; y < height; y += 1) {
    for (let x = 0; x < width; x += 1) {
      if (!isDiff(x, y)) continue;
      counted += 1;
      let n = 0;
      for (let dy = -1; dy <= 1; dy += 1) {
        for (let dx = -1; dx <= 1; dx += 1) {
          if ((dx || dy) && isDiff(x + dx, y + dy)) n += 1;
        }
      }
      if (n === 8) surrounded += 1;
    }
  }
  return { counted, surrounded };
}

// ---------------------------------------------------------------------- main

const server = await serve(PORT);
let session;
try {
  const url = `http://127.0.0.1:${PORT}/bench/capture.html?w=${W}&h=${H}&ratio=${RATIO}`;
  session = await launch({ ratio: RATIO });
  await session.call('Runtime.enable');
  await session.call('Page.enable');
  await session.call('Page.navigate', { url });
  const capture = JSON.parse(
    await poll(session.call, 'window.__capture ? JSON.stringify(window.__capture) : null'),
  );
  if (capture.error) throw new Error(`page reported: ${capture.error}`);

  const liveBytes = Buffer.from(capture.png.split(',')[1], 'base64');
  const live = decodePng(liveBytes);
  const nativeImg = decodePng(await readFile(path.join(REPO, 'examples/document.png')));

  console.log(`live   ${live.width}x${live.height}  (wasm, WebGL2 sparse strips)`);
  console.log(`native ${nativeImg.width}x${nativeImg.height}  (vello-hybrid, wgpu)`);

  if (live.width !== nativeImg.width || live.height !== nativeImg.height) {
    console.error(
      '\nMISMATCHED SIZE — re-render the reference at the same box:\n' +
        `  cargo run --example document_placeholder --features vello-hybrid,document-read,png -- ${W} ${H} ${RATIO}`,
    );
    process.exitCode = 1;
  } else {
    const d = diff(nativeImg.rgba, live.rgba, live.width, live.height);
    const shape = clumpiness(d.marks, live.width, live.height);
    const pct = ((d.differing / d.total) * 100).toFixed(3);

    console.log(`\ndiffering  ${d.differing} / ${d.total} px (${pct}%)`);
    console.log(`  by more than 1    ${d.overOne}`);
    console.log(`  max channel delta ${d.maxDelta}`);
    if (d.bbox) {
      console.log(
        `  bounding box      x ${d.bbox.minX}..${d.bbox.maxX}, y ${d.bbox.minY}..${d.bbox.maxY}`,
      );
    }
    console.log(
      `  fully surrounded  ${shape.surrounded} / ${shape.counted}` +
        ' (high means a region differs, not a fringe)',
    );
    const head = d.histogram.slice(0, 8).map(([k, v]) => `${k}:${v}`).join(' ');
    if (head) console.log(`  delta histogram   ${head}${d.histogram.length > 8 ? ' …' : ''}`);

    await writeFile(path.join(REPO, 'examples/document_diff.png'), encodePng(live.width, live.height, d.marks));
    await writeFile(path.join(REPO, 'examples/document_live.png'), liveBytes);
    console.log('\nwrote examples/document_diff.png and examples/document_live.png');
  }

  if (session.consoleLines.length) {
    console.log('\npage console:');
    for (const line of session.consoleLines) console.log(`  ${line}`);
  }
} finally {
  session?.done();
  server.kill();
}
