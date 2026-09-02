// Static server for the boot harness. Not `python3 -m http.server`, for two
// reasons that are the experiment rather than a convenience:
//
//   - The wasm has to arrive as `application/wasm`. Without it the glue falls
//     back from `WebAssembly.instantiateStreaming` to `arrayBuffer()` plus
//     `instantiate`, which is a different and slower boot.
//   - Whether the transport compresses decides several later questions —
//     whether shipping WOFF2 faces saves anything, and how much of the wasm's
//     3 MB is actually on the wire. So `--brotli` and `--no-compress` are
//     switches here rather than whatever the server happens to do.
//
// Serves the crate directory, so `/www/` and `/dist/` both resolve the way
// they do in the published layout.
//
//   node bench/server.mjs [--port 8080] [--brotli] [--no-cache]

import { createServer } from 'node:http';
import { createReadStream } from 'node:fs';
import { readFile, stat } from 'node:fs/promises';
import { brotliCompress, constants } from 'node:zlib';
import { promisify } from 'node:util';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const compress = promisify(brotliCompress);
const ROOT = path.resolve(fileURLToPath(new URL('..', import.meta.url)));

const args = process.argv.slice(2);
const flag = (name) => args.includes(`--${name}`);
const value = (name, fallback) => {
  const i = args.indexOf(`--${name}`);
  return i === -1 ? fallback : args[i + 1];
};

const PORT = Number(value('port', 8080));
const BROTLI = flag('brotli');
const NO_CACHE = flag('no-cache');
const WRONG_WASM_MIME = flag('wrong-wasm-mime');

const TYPES = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
  '.wasm': 'application/wasm',
  '.ttf': 'font/ttf',
  '.woff2': 'font/woff2',
  '.png': 'image/png',
  '.hep': 'application/octet-stream',
  '.ts': 'text/plain; charset=utf-8',
  '.txt': 'text/plain; charset=utf-8',
};

// Only text-ish and wasm payloads are worth compressing; a PNG or a TTF is
// already compressed and brotli-ing it measures nothing useful.
const COMPRESSIBLE = new Set(['.html', '.js', '.mjs', '.json', '.wasm', '.hep']);

// Compressed bodies are held rather than recomputed: quality 11 over 3 MB is
// seconds, and every run of the harness would pay it again.
const cache = new Map();

const server = createServer(async (req, res) => {
  try {
    const url = new URL(req.url, 'http://localhost');
    let rel = decodeURIComponent(url.pathname);
    if (rel.endsWith('/')) rel += 'index.html';
    const file = path.join(ROOT, path.normalize(rel));
    if (!file.startsWith(ROOT)) {
      res.writeHead(403).end('outside the served root');
      return;
    }

    const info = await stat(file).catch(() => null);
    if (!info?.isFile()) {
      res.writeHead(404).end(`no such file: ${rel}`);
      return;
    }

    const ext = path.extname(file);
    let type = TYPES[ext] ?? 'application/octet-stream';
    if (ext === '.wasm' && WRONG_WASM_MIME) type = 'application/octet-stream';

    const headers = {
      'content-type': type,
      'cache-control': NO_CACHE ? 'no-store' : 'public, max-age=3600',
      // The client's assets resolve relative to the module, so same-origin —
      // but a `<link rel=preload crossorigin>` in the page needs this to match
      // the request the fetch will make, or the preload is discarded and the
      // resource is fetched twice.
      'access-control-allow-origin': '*',
    };

    const wantsBrotli = BROTLI && COMPRESSIBLE.has(ext) && /\bbr\b/.test(req.headers['accept-encoding'] ?? '');
    if (wantsBrotli) {
      let body = cache.get(file);
      if (!body) {
        body = await compress(await readFile(file), {
          params: { [constants.BROTLI_PARAM_QUALITY]: 11 },
        });
        cache.set(file, body);
      }
      res.writeHead(200, {
        ...headers,
        'content-encoding': 'br',
        'content-length': String(body.length),
      });
      res.end(body);
      return;
    }

    res.writeHead(200, { ...headers, 'content-length': String(info.size) });
    createReadStream(file).pipe(res);
  } catch (e) {
    res.writeHead(500).end(String(e));
  }
});

server.listen(PORT, '127.0.0.1', () => {
  const how = [BROTLI ? 'brotli' : 'uncompressed', NO_CACHE ? 'no-store' : 'cacheable'];
  if (WRONG_WASM_MIME) how.push('wasm as octet-stream');
  console.log(`serving ${ROOT} on http://127.0.0.1:${PORT}/ (${how.join(', ')})`);
});
