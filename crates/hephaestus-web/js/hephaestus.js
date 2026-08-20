// Page-facing API for the hephaestus render client.
//
// The wasm module is deliberately imperative — render, resize, setDark — and
// everything browser-shaped lives here: ResizeObserver, matchMedia,
// requestAnimationFrame, fetch. JavaScript costs a page nothing to download
// twice over; wasm bytes are the thing worth being careful with.

import init, {
  PlotHandle,
  documentFormatVersion,
  hasFonts,
  isSupported,
  registerFont,
  setGenericFamily,
} from './hephaestus_web.js';

export {
  init as default,
  documentFormatVersion,
  hasFonts,
  isSupported,
  registerFont,
  setGenericFamily,
};

// Fonts are registered into a process-global context that lives as long as
// the module, so registering the same file twice is waste rather than an
// error. Tracking what has been asked for lets several plots on one page
// each request the font they need without refetching.
const registered = new Set();

/**
 * Fetch a font file and register it.
 *
 * Accepts TTF, OTF, TTC, OTC, WOFF and WOFF2 — the container formats are
 * unwrapped to the sfnt inside on the wasm side, so a URL from a font CDN
 * works as-is.
 *
 * **A variable font is the best thing to point this at.** The shaper applies
 * the `wght` axis, so one file serves every weight — including interpolated
 * ones a static set cannot reach — which means one call rather than one per
 * weight, and no per-face subset to choose. Italic is still a second file,
 * since it is a separate axis-space in practice.
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
 * Register a Google Fonts family. No API key needed.
 *
 * Uses the keyless CSS2 endpoint, which is possible because WOFF2 is decoded
 * (the `webfonts` feature). CORS allows the `fetch`, and the response is
 * WOFF2 whatever a page does — `User-Agent` is a forbidden header, so the
 * TTF the native `google-fonts` cargo feature relies on is unreachable here.
 *
 * **Exactly one file is registered per weight/style, by design.** Google
 * splits every face into per-script subset files sharing one family name, and
 * the shaper selects within a family by weight and style with no notion of CSS
 * `unicode-range` — so registering several subsets lets one without basic
 * Latin win and turns every label into tofu. `subset` picks which to take.
 * That cap is also why the bundled default font is a single file per face
 * covering five scripts: see {@link registerDefaultFonts}.
 *
 * @param {string} family e.g. `'Inter'`, `'Open Sans'`. Case-sensitive.
 * There is no "all subsets" option, and none is possible: the CSS endpoints
 * never serve a single full-coverage file. Asking for one weight returns seven
 * `unicode-range`-split files, and the legacy `&subset=` parameter no longer
 * merges them. For coverage beyond one script, use a **variable** font through
 * {@link registerFontFromUrl} — one file carries every weight, so it sidesteps
 * both this and the one-file-per-face cap — or the bundled default, which is
 * one file per face across five scripts.
 *
 * @param {{ weights?: number[], italics?: boolean, subset?: string,
 *           genericFor?: string }} [opts] `weights` defaults to `[400, 700]`
 *   and `italics` to `true`, which is what the theme and markdown chrome
 *   between them ask for. `subset` defaults to `'latin'`; it names one of the
 *   subsets the CSS response offers, and the error lists them if it misses.
 * @returns {Promise<string[]>} family names registered.
 */
export async function registerGoogleFont(family, opts = {}) {
  const weights = opts.weights ?? [400, 700];
  const italics = opts.italics !== false;
  const subset = opts.subset ?? 'latin';

  // ital,wght axis spec: 0 is upright, 1 italic, and the pairs must be sorted.
  const specs = [];
  for (const ital of italics ? [0, 1] : [0]) {
    for (const w of [...weights].sort((a, b) => a - b)) specs.push(`${ital},${w}`);
  }
  const url = new URL('https://fonts.googleapis.com/css2');
  url.searchParams.set('family', `${family}:ital,wght@${specs.join(';')}`);

  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(
      `Google Fonts CSS2 failed: ${response.status} ${response.statusText} ` +
        `for ${JSON.stringify(family)}`,
    );
  }
  const css = await response.text();

  // Each @font-face is preceded by a comment naming its subset.
  const blocks = [...css.matchAll(/\/\*\s*([\w-]+)\s*\*\/\s*@font-face\s*\{([^}]*)\}/g)]
    .map(([, name, body]) => ({
      subset: name,
      weight: (body.match(/font-weight:\s*(\d+)/) || [])[1],
      style: (body.match(/font-style:\s*(\w+)/) || [])[1],
      src: (body.match(/url\((https:[^)]+)\)/) || [])[1],
    }))
    .filter((b) => b.src);
  if (!blocks.length) {
    throw new Error(`no @font-face blocks for ${JSON.stringify(family)} — check the name`);
  }

  const wanted = blocks.filter((b) => b.subset === subset);
  if (!wanted.length) {
    const available = [...new Set(blocks.map((b) => b.subset))].join(', ');
    throw new Error(
      `${JSON.stringify(family)} has no ${JSON.stringify(subset)} subset; available: ${available}`,
    );
  }

  const names = new Set();
  for (const b of wanted) {
    for (const n of await registerFontFromUrl(b.src, {
      key: `google:${family}:${subset}:${b.style}:${b.weight}`,
    })) {
      names.add(n);
    }
  }
  const out = [...names];
  if (opts.genericFor && out.length) setGenericFamily(opts.genericFor, out);
  return out;
}

/**
 * The bundled faces, resolved against this module so a CDN copy finds its own.
 *
 * Four static instances rather than one variable font: `gvar` deltas survive
 * charset subsetting, so a variable roman/italic pair at this coverage is
 * about 1 MB against ~260 kB brotli for these four. The trade is that a theme
 * asking for weight 500 snaps to 400 or 700 rather than interpolating; the
 * built-in themes only use 400 and 700.
 */
const DEFAULT_FACES = ['regular', 'bold', 'italic', 'bolditalic'].map(
  (v) => new URL(`./fonts/roboto-${v}.ttf`, import.meta.url).href,
);

/** Family the bundled faces register under. */
const DEFAULT_FAMILY = 'Roboto';

/**
 * Register the bundled default font — Roboto, four faces.
 *
 * Covers latin, latin-ext, Greek, Cyrillic and Vietnamese, in regular, bold,
 * italic and bold-italic. All four matter: the theme sets a bold plot title,
 * and the rich-text sheet maps weight and italic independently, so markdown
 * chrome reaches every combination including nested `***emphasis***`. CJK is
 * deliberately absent — a CJK face is megabytes, and stays a bring-your-own
 * case.
 *
 * Fetched, not embedded in the wasm, so a page supplying its own font pays
 * nothing. Also points `sans-serif` at the family, without which a theme
 * naming the generic resolves to nothing.
 *
 * OFL-1.1; `fonts/OFL-Roboto.txt` ships alongside.
 */
export async function registerDefaultFonts() {
  const registered = await Promise.all(
    DEFAULT_FACES.map((url) => registerFontFromUrl(url, { key: url })),
  );
  // After the faces, so the name the mapping points at exists.
  setGenericFamily('sans-serif', [DEFAULT_FAMILY]);
  return [...new Set(registered.flat())];
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
   *   `defaultFont: false` suppresses fetching the bundled font when nothing
   *   else is registered, for a page that would rather render no text than
   *   pull ~260 kB.
   */
  static async create(canvas, doc, opts = {}) {
    if (!isSupported()) {
      throw new Error(
        'WebGPU is unavailable. This renderer rasterises through compute ' +
          'shaders, which WebGL2 cannot run, so there is no fallback path.',
      );
    }
    const bytes = doc instanceof Uint8Array ? doc : new Uint8Array(doc);

    // A browser enumerates no system fonts, so a page that has registered
    // none would render chrome with no text at all — no error, just missing
    // glyphs. Fetch the bundled default rather than let that happen, and skip
    // it entirely when anything is already registered. Before `create`, not
    // after: reading a document decodes its theme, which is already enough to
    // shape, so arriving late would mean a textless first frame.
    if (opts.defaultFont !== false && !hasFonts()) {
      await registerDefaultFonts();
    }

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
