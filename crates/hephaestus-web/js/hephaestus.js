// Page-facing API for the hephaestus render client.
//
// The wasm module is deliberately imperative — render, resize, setDark — and
// everything browser-shaped lives here: ResizeObserver, matchMedia,
// requestAnimationFrame, fetch. JavaScript costs a page nothing to download
// twice over; wasm bytes are the thing worth being careful with.

import init, {
  PlotHandle,
  documentFormatVersion,
  isSupported,
  registerFont,
  setGenericFamily,
} from './hephaestus_web.js';

export { init as default, documentFormatVersion, isSupported, registerFont, setGenericFamily };

// Fonts are registered into a process-global context that lives as long as
// the module, so registering the same file twice is waste rather than an
// error. Tracking what has been asked for lets several plots on one page
// each request the font they need without refetching.
const registered = new Set();

/**
 * Fetch a font file and register it.
 *
 * The URL must serve TTF, OTF, TTC or OTC. WOFF2 is rejected by the wasm
 * side — see `registerFont` — which matters here because that is what most
 * font CDNs hand a browser by default.
 *
 * @param {string} url
 * @param {{ genericFor?: string, key?: string }} [opts] `genericFor` also
 *   points that generic family at whatever families the file turned out to
 *   contain — which a theme asking for `sans-serif` needs, and which is why
 *   the family name comes back from `registerFont` rather than being guessed
 *   here. `key` overrides the dedupe key, normally the URL.
 * @returns {Promise<string[]>} family names registered, or `[]` if this URL
 *   was already registered.
 */
export async function registerFontFromUrl(url, opts = {}) {
  const key = opts.key ?? url;
  if (registered.has(key)) return [];

  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`font fetch failed: ${response.status} ${response.statusText} for ${url}`);
  }
  const families = registerFont(new Uint8Array(await response.arrayBuffer()));
  registered.add(key);
  if (opts.genericFor) setGenericFamily(opts.genericFor, families);
  return families;
}

/**
 * Register a Google Fonts family, via the Developer API.
 *
 * Uses `webfonts/v1`, whose `files` map gives `.ttf` URLs. The CSS2 API is
 * not usable from a page: it serves TTF only to clients sending no
 * `User-Agent` and WOFF2 to anything that looks like a browser, and
 * `User-Agent` is a forbidden header that `fetch` can neither remove nor
 * override. Hence the API key — there is no keyless route to TTF here.
 *
 * @param {string} family e.g. `'Inter'`, `'Open Sans'`. Case-sensitive.
 * @param {{ apiKey: string, variants?: string[], genericFor?: string }} opts
 * @returns {Promise<string[]>} family names registered, deduplicated.
 */
export async function registerGoogleFont(family, opts) {
  if (!opts?.apiKey) {
    throw new Error(
      'registerGoogleFont needs an apiKey (Google Fonts Developer API v1). ' +
        'For a keyless setup, self-host a TTF and use registerFontFromUrl.',
    );
  }
  const variants = opts.variants ?? ['regular'];

  const url = new URL('https://www.googleapis.com/webfonts/v1/webfonts');
  url.searchParams.set('family', family);
  url.searchParams.set('key', opts.apiKey);
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`Google Fonts API failed: ${response.status} ${response.statusText}`);
  }
  const item = (await response.json()).items?.[0];
  if (!item) throw new Error(`Google Fonts has no family named ${JSON.stringify(family)}`);

  const families = new Set();
  for (const variant of variants) {
    const file = item.files?.[variant];
    if (!file) {
      throw new Error(
        `family ${JSON.stringify(family)} has no ${JSON.stringify(variant)} variant; ` +
          `available: ${Object.keys(item.files ?? {}).join(', ')}`,
      );
    }
    // The API returns http:// for some families; a page on https can't fetch it.
    for (const name of await registerFontFromUrl(file.replace(/^http:/, 'https:'), {
      key: `google:${family}:${variant}`,
    })) {
      families.add(name);
    }
  }
  const names = [...families];
  if (opts.genericFor && names.length) setGenericFamily(opts.genericFor, names);
  return names;
}

const CRC_TABLE = (() => {
  const t = new Int32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c;
  }
  return t;
})();

function crc32(bytes) {
  let c = -1;
  for (let i = 0; i < bytes.length; i++) c = CRC_TABLE[(c ^ bytes[i]) & 0xff] ^ (c >>> 8);
  return (c ^ -1) >>> 0;
}

/**
 * Return `png` with a `pHYs` chunk declaring `dpi`, replacing any already
 * there, or `null` when it could not be applied and the input should stand.
 *
 * A canvas exports no physical resolution, so every viewer falls back to its
 * own default — 72 dpi typically — and a plot rendered at 2× device pixels
 * then claims to be twice its intended physical size. `pHYs` is the chunk that
 * records it, and the browser will not write one, so it is spliced in here.
 */
function withPngDpi(png, dpi) {
  if (!(dpi > 0)) return null;
  // PNG signature is 8 bytes; IHDR always follows and must stay first.
  if (png.length < 8 || png[0] !== 0x89 || png[1] !== 0x50) return null;
  const ppm = Math.round(dpi / 0.0254); // pixels per metre, pHYs unit 1

  const body = new Uint8Array(13);
  body.set([0x70, 0x48, 0x59, 0x73]); // "pHYs"
  const dv = new DataView(body.buffer);
  dv.setUint32(4, ppm);
  dv.setUint32(8, ppm);
  body[12] = 1; // unit: metre
  const chunk = new Uint8Array(21);
  new DataView(chunk.buffer).setUint32(0, 9); // length of the chunk data
  chunk.set(body, 4);
  new DataView(chunk.buffer).setUint32(17, crc32(body));

  // Walk chunks so an existing pHYs is dropped rather than duplicated, and so
  // the new one lands after IHDR where the spec requires it.
  const out = [png.subarray(0, 8)];
  let i = 8;
  let inserted = false;
  const view = new DataView(png.buffer, png.byteOffset, png.byteLength);
  while (i + 8 <= png.length) {
    const len = view.getUint32(i);
    const type = String.fromCharCode(png[i + 4], png[i + 5], png[i + 6], png[i + 7]);
    const end = i + 12 + len;
    if (type !== 'pHYs') out.push(png.subarray(i, end));
    i = end;
    if (type === 'IHDR' && !inserted) {
      out.push(chunk);
      inserted = true;
    }
  }
  if (!inserted) return null;
  const total = out.reduce((n, a) => n + a.length, 0);
  const merged = new Uint8Array(total);
  let at = 0;
  for (const part of out) {
    merged.set(part, at);
    at += part.length;
  }
  return merged;
}

/**
 * Encode bytes back to a `data:image/png;base64,` URL.
 *
 * A blob URL would be cheaper, but Safari's image context menu degrades for
 * `blob:` — the save entries are replaced by a single "Get Picture" — so the
 * scheme the bytes arrive under is load-bearing for the right-click
 * affordance. Chunked because `String.fromCharCode(...bytes)` overflows the
 * argument limit on anything megabyte-sized.
 */
function bytesToPngDataUrl(bytes) {
  let binary = '';
  const STEP = 0x8000;
  for (let i = 0; i < bytes.length; i += STEP) {
    binary += String.fromCharCode.apply(null, bytes.subarray(i, i + STEP));
  }
  return 'data:image/png;base64,' + btoa(binary);
}

/** Decode a `data:...;base64,` URL to bytes. */
function dataUrlToBytes(url) {
  const b64 = url.slice(url.indexOf(',') + 1);
  const raw = atob(b64);
  const out = new Uint8Array(raw.length);
  for (let i = 0; i < raw.length; i++) out[i] = raw.charCodeAt(i);
  return out;
}

/**
 * A plot document bound to a canvas, with resize and color-scheme handling.
 *
 * Frames are coalesced: every mutator marks the view dirty and schedules one
 * animation frame, so a resize and a theme change in the same tick cost one
 * render. `redraw()` forces one immediately.
 */
export class PlotView {
  /**
   * @param {HTMLCanvasElement} canvas
   * @param {Uint8Array|ArrayBuffer} doc bytes of a `.hplot` document.
   * @param {{ colorScheme?: 'light'|'dark'|'auto', autoResize?: boolean,
   *           picking?: boolean, saveOnRightClick?: boolean|string }} [opts]
   *   `picking` allocates a second render target and reads it back after
   *   every frame, so leave it off unless `pickAt` is going to be called.
   *   `saveOnRightClick` gives the canvas an ordinary image's context menu.
   *   The browser chooses the suggested filename. See
   *   {@link PlotView#_installImageOverlay}.
   */
  static async create(canvas, doc, opts = {}) {
    if (!isSupported()) {
      throw new Error(
        'WebGPU is unavailable. This renderer rasterises through compute ' +
          'shaders, which WebGL2 cannot run, so there is no fallback path.',
      );
    }
    const bytes = doc instanceof Uint8Array ? doc : new Uint8Array(doc);
    const handle = await PlotHandle.create(canvas, bytes, opts.picking === true);
    return new PlotView(canvas, handle, opts);
  }

  /** @private — use {@link PlotView.create}. */
  constructor(canvas, handle, opts) {
    this.canvas = canvas;
    this.handle = handle;
    this._frame = null;
    this._observer = null;
    this._media = null;
    this._onMedia = null;
    this._overlay = null;
    this._refreshOverlay = null;
    // Kept in step with `_applySize` so an exported PNG can declare the
    // resolution it was actually rendered at.
    this._dpi = 96 * (window.devicePixelRatio || 1);
    this._freed = false;

    this.setColorScheme(opts.colorScheme ?? 'light');

    if (opts.autoResize !== false) {
      // ResizeObserver rather than a window resize listener: the canvas can
      // change size from a flex reflow or a sibling appearing, neither of
      // which resizes the window.
      this._observer = new ResizeObserver((entries) => this._onObserved(entries));
      try {
        // device-pixel-content-box reports the exact device-pixel box, so the
        // backing store matches it with no rounding drift against
        // devicePixelRatio. Not universally supported — observe() throws where
        // it isn't, and the CSS box times the ratio is the fallback.
        this._observer.observe(this.canvas, { box: 'device-pixel-content-box' });
      } catch {
        this._observer.observe(this.canvas);
      }
    }
    this._syncSize();

    if (opts.saveOnRightClick) {
      this._installImageOverlay();
    }
  }

  /**
   * Give the canvas the context menu an ordinary image has.
   *
   * A `<canvas>` never offers "Save image as…" / "Copy image" / drag-to-save:
   * those come from the hit-test node being an image, and a canvas is not
   * one. Nor can a `contextmenu` handler retarget the menu. So the only way
   * to get the *native* affordance — rather than a bespoke menu that looks
   * nothing like the browser's — is for the right-click to genuinely land on
   * an `<img>`.
   *
   * This overlays a transparent image exactly on the canvas and keeps its
   * `src` refreshed from the live frame, so the canvas below stays the thing
   * that renders while the image above is the thing the menu acts on.
   *
   * Encoding is lazy: a PNG per frame would be pointless work, so the src is
   * refreshed only when a menu is plausibly about to open — pointer entry,
   * a right/ctrl mousedown, and `contextmenu` itself. `mousedown` matters
   * because a browser gathers the menu's image URL from the hit test, which
   * can precede the `contextmenu` dispatch.
   */
  _installImageOverlay() {
    const parent = this.canvas.parentElement;
    if (!parent) return;
    // The overlay is positioned against the canvas's containing block, so
    // that block has to be positioned. Only touched when it isn't already.
    if (getComputedStyle(parent).position === 'static') parent.style.position = 'relative';

    const img = document.createElement('img');
    img.alt = '';
    // Invisible but hit-testable: the canvas underneath is what the viewer
    // sees, and `opacity` (rather than `visibility` or `display`) is what
    // keeps the element in the hit-test tree.
    img.style.cssText =
      'position:absolute;inset:0;width:100%;height:100%;opacity:0;' +
      'pointer-events:auto;touch-action:none';
    parent.appendChild(img);
    this._overlay = img;

    const refresh = () => {
      if (this._freed) return;
      try {
        // Draw first, always. Safari returns an all-black snapshot unless a
        // render has happened recently — its drawable is consumed once
        // composited — where Chrome and Firefox retain the presented image.
        // Verified: capturing without this is black in Safari and correct
        // with it.
        this._renderNow();
        const raw = this.canvas.toDataURL('image/png');
        const png = withPngDpi(dataUrlToBytes(raw), this._dpi);
        // Back to a data URL rather than a blob: Safari's image context menu
        // loses its save entries for `blob:` sources. Reuse the canvas's own
        // string when the patch was a no-op.
        img.src = png === null ? raw : bytesToPngDataUrl(png);
      } catch {
        // A tainted or zero-sized canvas; leave the previous src alone.
      }
    };
    this._refreshOverlay = refresh;
    refresh();

    img.addEventListener('pointerenter', refresh);
    img.addEventListener('mousedown', (e) => {
      // Right-click, or macOS ctrl-click.
      if (e.button === 2 || e.ctrlKey) refresh();
    });
    img.addEventListener('contextmenu', refresh);
  }

  /** Draw immediately, cancelling any frame already scheduled. */
  redraw() {
    this._renderNow();
  }

  /**
   * Set the drawing surface size explicitly, in CSS pixels.
   *
   * Only needed when `autoResize` is off, or to drive the size from
   * something the observer cannot see.
   */
  resize(cssWidth, cssHeight, ratio = window.devicePixelRatio || 1) {
    if (this._freed) return;
    this._applySize(
      Math.max(1, Math.round(cssWidth * ratio)),
      Math.max(1, Math.round(cssHeight * ratio)),
      ratio,
    );
    // Synchronously, not on the next frame: see `_applySize`.
    this._renderNow();
  }

  /**
   * Choose the theme: as authored, inverted, or following the OS.
   *
   * `'auto'` attaches a `prefers-color-scheme` listener and re-renders when
   * it changes. Inversion swaps the palette's paper and ink anchors, so
   * chrome follows; a geom given an explicit color keeps it.
   *
   * @param {'light'|'dark'|'auto'} scheme
   */
  setColorScheme(scheme) {
    if (this._freed) return;
    if (!['light', 'dark', 'auto'].includes(scheme)) {
      throw new Error(`unknown color scheme ${JSON.stringify(scheme)}`);
    }
    this._detachMedia();
    this._scheme = scheme;

    if (scheme === 'auto') {
      this._media = window.matchMedia('(prefers-color-scheme: dark)');
      this._onMedia = (e) => {
        this.handle.setDark(e.matches);
        this._schedule();
      };
      this._media.addEventListener('change', this._onMedia);
      this.handle.setDark(this._media.matches);
    } else {
      this.handle.setDark(scheme === 'dark');
    }
    this._schedule();
  }

  /** The scheme last asked for — `'auto'` if it is following the OS. */
  colorScheme() {
    return this._scheme;
  }

  /** Whether the inverted theme is currently drawn. Resolves `'auto'`. */
  isDark() {
    return this.handle.isDark();
  }

  /**
   * The row id under a point, or `undefined` for empty space.
   *
   * Takes **CSS** pixels — an event's `offsetX` / `offsetY` — and scales them
   * to the backing store itself, which is the conversion every caller would
   * otherwise get wrong on a high-density display.
   *
   * Returns `undefined` unless the view was created with `picking: true`.
   * The hitmap may lag the visible frame slightly, since the readback is
   * never waited on.
   */
  pickAt(cssX, cssY) {
    if (this._freed) return undefined;
    const ratio = this.canvas.width / (this.canvas.clientWidth || this.canvas.width);
    return this.handle.pickAt(Math.round(cssX * ratio), Math.round(cssY * ratio));
  }

  /**
   * The size and dpi the document's writer recorded, if any.
   *
   * Advisory. Useful as an aspect ratio for a container that must be sized
   * before the plot is laid out.
   */
  hints() {
    return {
      width: this.handle.hintWidth(),
      height: this.handle.hintHeight(),
      dpi: this.handle.hintDpi(),
    };
  }

  /** Detach observers and release the wasm-side handle. */
  free() {
    if (this._freed) return;
    this._freed = true;
    if (this._frame !== null) cancelAnimationFrame(this._frame);
    this._observer?.disconnect();
    this._detachMedia();
    this._overlay?.remove();
    this._overlay = null;
    this.handle.free();
  }

  _onObserved(entries) {
    if (this._freed) return;
    const entry = entries[entries.length - 1];
    const ratio = window.devicePixelRatio || 1;
    const exact = entry.devicePixelContentBoxSize?.[0];
    if (exact) {
      this._applySize(
        Math.max(1, exact.inlineSize),
        Math.max(1, exact.blockSize),
        ratio,
      );
    } else {
      // Reading the entry rather than clientWidth avoids forcing a layout
      // flush inside the callback.
      const box = entry.contentBoxSize?.[0];
      const cssW = box ? box.inlineSize : this.canvas.clientWidth;
      const cssH = box ? box.blockSize : this.canvas.clientHeight;
      this._applySize(
        Math.max(1, Math.round(cssW * ratio)),
        Math.max(1, Math.round(cssH * ratio)),
        ratio,
      );
    }
    this._renderNow();
  }

  _syncSize() {
    // clientWidth is 0 for a canvas that isn't laid out (display:none, or
    // detached); fall back to the attributes so the surface stays valid.
    const w = this.canvas.clientWidth || this.canvas.width;
    const h = this.canvas.clientHeight || this.canvas.height;
    this.resize(w, h);
  }

  /**
   * Point the backing store and the renderer at a device-pixel size.
   *
   * Assigning `canvas.width` / `canvas.height` **clears the drawing buffer**,
   * so every caller has to draw again before the browser next paints or a
   * blank frame reaches the screen. That is why nothing here defers a
   * post-resize draw to `requestAnimationFrame`: a `ResizeObserver` callback
   * runs after layout but *before* paint, so drawing in the same turn lands
   * the new frame in the same paint, while an rAF lands it one paint later —
   * with the cleared buffer shown in between.
   */
  _applySize(width, height, ratio) {
    if (this.canvas.width !== width) this.canvas.width = width;
    if (this.canvas.height !== height) this.canvas.height = height;
    this._dpi = 96 * ratio;
    this.handle.resize(width, height, ratio);
  }

  /** Draw now, dropping any frame already scheduled. */
  _renderNow() {
    if (this._freed) return;
    if (this._frame !== null) {
      cancelAnimationFrame(this._frame);
      this._frame = null;
    }
    this.handle.render();
  }

  /**
   * Coalesce a draw into the next frame.
   *
   * For changes that do *not* clear the backing store — a theme swap — where
   * batching several mutations into one frame is worth a frame of latency.
   */
  _schedule() {
    if (this._freed || this._frame !== null) return;
    this._frame = requestAnimationFrame(() => {
      this._frame = null;
      if (!this._freed) this.handle.render();
    });
  }

  _detachMedia() {
    if (this._media && this._onMedia) {
      this._media.removeEventListener('change', this._onMedia);
    }
    this._media = null;
    this._onMedia = null;
  }
}
