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
} from './hephaestus_wasm.js';

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
   *           picking?: boolean, saveOnRightClick?: boolean|string,
   *           defaultFont?: boolean, placeholder?: HTMLImageElement|string }} [opts]
   *   `picking` allocates a second render target and reads it back after
   *   every frame, so leave it off unless `pickAt` is going to be called.
   *   `saveOnRightClick` gives the canvas an ordinary image's context menu.
   *   Pass a string to name the saved file — a bare `true` uses `plot.png`.
   *   The name is a hint: a `data:` URL has no path for a browser to take a
   *   filename from, so it travels as a media-type parameter and an engine
   *   that ignores it falls back to whatever it would have chosen (Safari
   *   says "Unknown"). See {@link PlotView#_enableSaveAffordance}.
   *   `defaultFont: false` suppresses fetching the bundled font when nothing
   *   else is registered, for a page that would rather render no text than
   *   pull ~260 kB.
   *   `placeholder` is a static image to show until the first live frame
   *   lands. An `HTMLImageElement` already in the page is adopted, which is
   *   the form that reaches the *first* paint: a producer that can rasterise
   *   the plot itself puts the picture in the served HTML and the viewer sees
   *   it before any script runs. A string is a URL for an element created
   *   here, which cannot beat the renderer to the screen but is the
   *   convenient form for a lazy embed. Either way it becomes the
   *   `saveOnRightClick` overlay once the live frame replaces it, so the two
   *   share one node. Nothing here touches it until there is a frame to
   *   reveal, so a renderer that never starts leaves the picture on screen.
   */
  static async create(canvas, doc, opts = {}) {
    if (!isSupported()) {
      throw new Error(
        'This browser cannot run the renderer. The default build needs a ' +
          'WebGL2 context; a bundle built with the wgpu backend needs WebGPU ' +
          'instead. Serve a static image as the fallback.',
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
    const view = new PlotView(canvas, handle, opts);
    // The constructor drew frame 1 synchronously and the browser has not
    // painted since, so retiring here puts the reveal and the hide in one
    // paint. Deferring to an animation frame would show the placeholder once
    // more with the live frame already underneath.
    view._retirePlaceholder();
    return view;
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
    // Whether the overlay is still showing the host's picture. Until it is
    // retired the element is opaque and the canvas beneath it is unseen, and
    // the `error` handler and the save affordance both behave differently.
    this._showingPlaceholder = false;
    this._saveAs = null;
    // Kept in step with `_applySize` so an exported PNG can declare the
    // resolution it was actually rendered at.
    this._dpi = 96 * (window.devicePixelRatio || 1);
    this._freed = false;

    if (opts.saveOnRightClick) {
      this._saveAs = typeof opts.saveOnRightClick === 'string' ? opts.saveOnRightClick : 'plot.png';
    }
    // Before the first draw, so the picture is in place if anything below
    // throws — and so an adopted element is never removed and re-added.
    this._resolveOverlay(opts.placeholder);

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

    if (this._saveAs !== null) this._enableSaveAffordance(this._saveAs);
  }

  /**
   * Put the overlay image in place, adopting one the page already has.
   *
   * One element serves two purposes in sequence. While it is opaque it is the
   * placeholder — a picture of this plot, on screen before the renderer
   * exists. Once {@link PlotView#_retirePlaceholder} drops it to transparent
   * it is the save affordance, because a `<canvas>` never offers "Save image
   * as…" / "Copy image" / drag-to-save: those come from the hit-test node
   * being an image, and a `contextmenu` handler cannot retarget the menu. So
   * the only way to get the *native* affordance — rather than a bespoke menu
   * that looks nothing like the browser's — is for the right-click to
   * genuinely land on an `<img>`.
   *
   * `placeholder` is an element to adopt, a URL to create one from, or
   * nothing. With nothing and no `saveOnRightClick` there is no overlay at
   * all — it would sit over the canvas swallowing the pointer events a page
   * doing its own hit testing needs. With nothing and a filename the element
   * is created transparent, as the save affordance alone.
   */
  _resolveOverlay(placeholder) {
    const wanted = placeholder != null && placeholder !== '';
    if (!wanted && this._saveAs === null) return;
    const parent = this.canvas.parentElement;
    if (!parent) return;
    // The overlay is positioned against the canvas's containing block, so
    // that block has to be positioned. Only touched when it isn't already.
    if (getComputedStyle(parent).position === 'static') parent.style.position = 'relative';

    let img = null;
    let adopted = false;
    if (placeholder instanceof HTMLImageElement) {
      img = placeholder;
      adopted = true;
    } else if (typeof placeholder === 'string' && placeholder !== '') {
      img = document.createElement('img');
      img.alt = '';
      img.src = placeholder;
    } else {
      img = document.createElement('img');
      img.alt = '';
    }

    // Invisible but hit-testable once retired: the canvas underneath is what
    // the viewer sees, and `opacity` (rather than `visibility` or `display`)
    // is what keeps the element in the hit-test tree. Opaque until then.
    this._showingPlaceholder = wanted;
    img.style.position = 'absolute';
    img.style.inset = '0';
    img.style.width = '100%';
    img.style.height = '100%';
    img.style.pointerEvents = 'auto';
    img.style.touchAction = 'none';
    img.style.opacity = this._showingPlaceholder ? '1' : '0';

    img.addEventListener('error', () => this._onOverlayError(img));
    // Last child, so it stacks above the canvas without needing a z-index.
    // Re-appending an adopted element moves it, which is what we want.
    parent.appendChild(img);
    this._overlay = img;
    // An adopted element carries a description the viewer can actually use
    // while the picture is showing; one created here describes nothing.
    if (!adopted) img.alt = '';
  }

  /**
   * Refresh the overlay's `src` from the live frame when a menu is plausibly
   * about to open.
   *
   * Encoding is lazy: a PNG per frame would be pointless work, so the src is
   * refreshed only on pointer entry, a right/ctrl mousedown, and
   * `contextmenu` itself. `mousedown` matters because a browser gathers the
   * menu's image URL from the hit test, which can precede the `contextmenu`
   * dispatch.
   */
  _enableSaveAffordance(filename) {
    const img = this._overlay;
    if (!img) return;

    const refresh = () => {
      if (this._freed) return;
      try {
        // Draw first, always, and synchronously — nothing may await between
        // the render and the capture. Two separate reasons converge on it:
        // Safari returns an all-black snapshot unless a render happened
        // recently, its drawable being consumed once composited; and the
        // WebGL2 build's context has no `preserveDrawingBuffer`, so its
        // drawing buffer is cleared after compositing too. Rendering in the
        // same task as the capture satisfies both.
        this._renderNow();
        const raw = this.canvas.toDataURL('image/png');
        const png = withPngDpi(dataUrlToBytes(raw), this._dpi);
        // Back to a data URL rather than a blob: Safari's image context menu
        // loses its save entries for `blob:` sources. Reuse the canvas's own
        // string when the patch was a no-op.
        // Name the resource. A `data:` URL has no path for a browser to take
        // a filename from, which is why Safari offers "Unknown"; a `name`
        // media-type parameter is the only hint the URL can carry. Ignored by
        // browsers that do not read it, so it costs nothing to send.
        const url = png === null ? raw : bytesToPngDataUrl(png);
        img.src = this._nameRejected ? url : named(url, filename);
      } catch (e) {
        // A tainted or zero-sized canvas is the expected reason, and the
        // previous src is the right thing to keep. But swallowing this
        // silently once hid a genuinely broken capture path for a long time —
        // the overlay simply never got a src, and a right-click showed the
        // wrong menu with nothing logged. Report the first failure and stay
        // quiet after that, since the handlers fire on every pointer entry.
        if (!this._overlayWarned) {
          this._overlayWarned = true;
          console.warn('hephaestus: could not refresh the save overlay', e);
        }
      }
    };
    this._refreshOverlay = refresh;
    // Not while the placeholder is up: the host's picture is already a
    // correct image of this plot, so it serves as the initial src and the
    // first pointer entry upgrades it. That keeps a render plus a full PNG
    // encode off the path to the first frame.
    if (!this._showingPlaceholder) refresh();

    img.addEventListener('pointerenter', refresh);
    img.addEventListener('mousedown', (e) => {
      // Right-click, or macOS ctrl-click.
      if (e.button === 2 || e.ctrlKey) refresh();
    });
    img.addEventListener('contextmenu', refresh);
  }

  /**
   * Reveal the canvas, and leave the overlay as whatever it is still for.
   *
   * Called in the same task as the first draw, so one paint both shows the
   * live frame and hides the picture over it. There is deliberately no
   * transition: the two images agree pixel for pixel, and blending them
   * would show a ghost where a hard cut shows nothing.
   */
  _retirePlaceholder() {
    if (!this._showingPlaceholder) return;
    this._showingPlaceholder = false;
    const img = this._overlay;
    if (!img) return;
    if (this._saveAs === null) {
      // Nothing else wanted the element.
      img.remove();
      this._overlay = null;
      return;
    }
    img.style.opacity = '0';
    // The host's picture is now the save overlay's src, so give it the
    // filename hint the affordance would have. A prefix rewrite on a data URL
    // and a no-op on anything else, and the element is already invisible so
    // a re-decode cannot flicker.
    if (!this._nameRejected && img.src.startsWith('data:')) {
      img.src = named(img.src, this._saveAs);
    }
  }

  /**
   * A failed overlay load, handled according to which phase it is in.
   *
   * While the picture is showing, a failure would put broken-image chrome
   * over a canvas nobody has seen yet, so get out of the way at once and let
   * the plot arrive as it would with no placeholder. Afterwards, the only
   * expected cause is an engine rejecting the `name` media-type parameter, so
   * fall back to the same bytes unnamed — an overlay that fails to load is
   * not an image, and a right-click would silently get the ordinary element
   * menu instead, which is the failure this guards.
   */
  _onOverlayError(img) {
    if (this._showingPlaceholder) {
      this._showingPlaceholder = false;
      img.style.opacity = '0';
      if (this._saveAs === null) {
        img.remove();
        if (this._overlay === img) this._overlay = null;
      } else if (this._refreshOverlay) {
        this._refreshOverlay();
      }
      return;
    }
    const plain = img.src.replace(/;name=[^;,]*/, '');
    if (plain !== img.src) {
      this._nameRejected = true;
      img.src = plain;
    }
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

  /**
   * Detach observers and release the wasm-side handle.
   *
   * Removes the overlay image, including a `placeholder` element the page
   * supplied: adopting one transfers ownership of it.
   */
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

// ─── PNG dpi patching ───────────────────────────────────────────────────────
//
// A canvas writes no `pHYs` chunk, so a viewer assumes 72 dpi and a plot
// rendered at 2x device pixels claims twice its physical size. These three
// helpers exist to fix that on the way from `toDataURL` to the overlay image:
// decode the data URL, insert or replace `pHYs`, re-encode.

/** Bytes behind a `data:` URL. Throws if it is not base64-encoded. */
function dataUrlToBytes(dataUrl) {
  const comma = dataUrl.indexOf(',');
  if (comma < 0 || !dataUrl.slice(0, comma).includes(';base64')) {
    throw new Error('not a base64 data URL');
  }
  const raw = atob(dataUrl.slice(comma + 1));
  const bytes = new Uint8Array(raw.length);
  for (let i = 0; i < raw.length; i += 1) bytes[i] = raw.charCodeAt(i);
  return bytes;
}

/** A `data:image/png;base64,` URL for `bytes`. */
function bytesToPngDataUrl(bytes) {
  // Chunked because `String.fromCharCode(...bytes)` blows the argument limit
  // on anything bigger than a thumbnail.
  let ascii = '';
  const step = 0x8000;
  for (let i = 0; i < bytes.length; i += step) {
    ascii += String.fromCharCode.apply(null, bytes.subarray(i, i + step));
  }
  return `data:image/png;base64,${btoa(ascii)}`;
}

/**
 * Add a `name` parameter to a `data:` URL's media type.
 *
 * Rewrites only the prefix, so the base64 payload is never re-encoded — this
 * runs on pointer entry and the payload is the expensive part. A name that
 * already looks right is left alone.
 */
function named(dataUrl, filename) {
  const comma = dataUrl.indexOf(',');
  if (comma < 0) return dataUrl;
  const safe = sanitiseFilename(filename);
  if (!safe) return dataUrl;
  let prefix = dataUrl.slice(0, comma);
  if (prefix.includes(';name=')) return dataUrl;
  // `;base64` has to stay immediately before the comma.
  prefix = prefix.replace(/;base64$/, `;name=${safe};base64`);
  return prefix + dataUrl.slice(comma);
}

/**
 * Reduce `filename` to something safe to put in a URL parameter, or `null`.
 *
 * Path separators and quotes are dropped rather than escaped: this ends up in
 * a header-ish position and in a save dialog, and neither wants a path.
 */
function sanitiseFilename(filename) {
  if (typeof filename !== 'string') return null;
  const base = filename.split(/[\\/]/).pop().replace(/[^\w.\- ]/g, '').trim();
  if (!base || base === '.' || base === '..') return null;
  return /\.png$/i.test(base) ? base : `${base}.png`;
}

const PNG_SIGNATURE = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];

let crcTable = null;

/** CRC-32 as PNG defines it, over `bytes[from..to)`. */
function crc32(bytes, from, to) {
  if (crcTable === null) {
    crcTable = new Int32Array(256);
    for (let n = 0; n < 256; n += 1) {
      let c = n;
      for (let k = 0; k < 8; k += 1) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
      crcTable[n] = c;
    }
  }
  let c = 0xffffffff;
  for (let i = from; i < to; i += 1) c = crcTable[(c ^ bytes[i]) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

/**
 * Return `png` with a `pHYs` chunk describing `dpi`, or `null` when the bytes
 * already say that.
 *
 * `null` rather than a copy so the caller can reuse the string it already has
 * — re-encoding a megabyte of base64 to change nothing is the one cost worth
 * avoiding on a path that runs on pointer entry.
 */
function withPngDpi(png, dpi) {
  for (let i = 0; i < PNG_SIGNATURE.length; i += 1) {
    if (png[i] !== PNG_SIGNATURE[i]) throw new Error('not a PNG');
  }
  // `pHYs` counts pixels per metre, and one inch is exactly 0.0254 m.
  const perMetre = Math.round(dpi / 0.0254);

  const chunk = new Uint8Array(21);
  const view = new DataView(chunk.buffer);
  view.setUint32(0, 9); // data length
  chunk[4] = 0x70; // 'p'
  chunk[5] = 0x48; // 'H'
  chunk[6] = 0x59; // 'Y'
  chunk[7] = 0x73; // 's'
  view.setUint32(8, perMetre);
  view.setUint32(12, perMetre);
  chunk[16] = 1; // unit specifier: metre
  view.setUint32(17, crc32(chunk, 4, 17));

  // Walk the chunk list to find where `pHYs` belongs: after IHDR, and before
  // the first IDAT. An existing one is replaced rather than duplicated.
  let offset = PNG_SIGNATURE.length;
  let insertAt = -1;
  let replaceEnd = -1;
  while (offset + 8 <= png.length) {
    const len = new DataView(png.buffer, png.byteOffset + offset, 4).getUint32(0);
    const type = String.fromCharCode(png[offset + 4], png[offset + 5], png[offset + 6], png[offset + 7]);
    const total = 12 + len;
    if (type === 'pHYs') {
      // Already correct? Then the caller keeps what it has.
      let same = true;
      for (let i = 0; i < 21; i += 1) {
        if (png[offset + i] !== chunk[i]) {
          same = false;
          break;
        }
      }
      if (same) return null;
      insertAt = offset;
      replaceEnd = offset + total;
      break;
    }
    if (type === 'IDAT' || type === 'IEND') {
      insertAt = offset;
      replaceEnd = offset;
      break;
    }
    offset += total;
  }
  if (insertAt < 0) throw new Error('PNG has no IDAT');

  const out = new Uint8Array(png.length - (replaceEnd - insertAt) + chunk.length);
  out.set(png.subarray(0, insertAt), 0);
  out.set(chunk, insertAt);
  out.set(png.subarray(replaceEnd), insertAt + chunk.length);
  return out;
}
