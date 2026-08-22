/**
 * Render a hephaestus plot document onto a canvas in the browser.
 *
 * A document carries a plot's configuration rather than a picture of it, so
 * the page re-solves the layout at whatever size it has: resizing reflows —
 * axes re-lay-out, ticks recompute, text re-wraps — instead of stretching.
 */

/** Instantiate the wasm module. Await this before anything else. */
export default function init(
  module_or_path?: { module_or_path: unknown } | unknown,
): Promise<unknown>;

/**
 * Whether this browser can run the renderer.
 *
 * What it tests depends on which backend the bundle was built with. The
 * default needs only a WebGL2 context, so this is very nearly always `true`;
 * a bundle built with the wgpu backend needs WebGPU, where it is a hard gate.
 * Either way, serve a static image when it is `false`.
 */
export function isSupported(): boolean;

/**
 * Major version of the plot-document format this build reads.
 *
 * A document whose major differs is refused outright, so assert this against
 * whatever writes your documents rather than discovering it as a plot that
 * never appears.
 */
export function documentFormatVersion(): number;

/**
 * Register every font face in `bytes`, returning the family names they landed
 * under. Accepts sfnt (TTF/OTF/TTC/OTC) and WOFF/WOFF2, which are unwrapped
 * first. Throws on bytes holding no recognisable face.
 *
 * A browser enumerates no system fonts, so a page that registers none renders
 * chrome with no text. Call before creating any view. Registration is global
 * and permanent: once per page, not once per plot.
 */
export function registerFont(bytes: Uint8Array): string[];

/**
 * Point a generic family at concrete families already registered.
 *
 * `kind` is one of `serif`, `sans-serif`, `monospace`, `cursive`, `fantasy`,
 * `system-ui`. Registering a font is not enough on its own — a theme asking
 * for `sans-serif` resolves to nothing until this says what it means here.
 */
export function setGenericFamily(kind: string, families: string[]): void;

/** Fetch a font and register it. TTF/OTF/TTC/OTC/WOFF/WOFF2. */
export function registerFontFromUrl(
  url: string,
  opts?: { genericFor?: string; key?: string },
): Promise<string[]>;

/**
 * Register a Google Fonts family. No API key needed — the keyless CSS2
 * endpoint works because WOFF2 is decoded.
 *
 * Registers exactly one file per weight/style: Google splits faces into
 * per-script subsets sharing a family name, and the shaper has no notion of
 * CSS `unicode-range`, so mixing them turns labels into tofu. `subset`
 * chooses which; it defaults to `'latin'`.
 */
export function registerGoogleFont(
  family: string,
  opts?: { weights?: number[]; italics?: boolean; subset?: string; genericFor?: string },
): Promise<string[]>;

/** Whether any font family is available to shape with. */
export function hasFonts(): boolean;

/**
 * Register the bundled default font: Roboto, four faces (regular, bold,
 * italic, bold-italic), covering latin, latin-ext, Greek, Cyrillic and
 * Vietnamese. Fetched from the package, not embedded in the wasm. OFL-1.1.
 *
 * `PlotView.create` calls this for you when nothing else is registered.
 */
export function registerDefaultFonts(): Promise<string[]>;

export interface PlotViewOptions {
  /** Theme to draw with. `'auto'` follows `prefers-color-scheme`. */
  colorScheme?: 'light' | 'dark' | 'auto';
  /** Track the canvas's CSS box with a ResizeObserver. Default `true`. */
  autoResize?: boolean;
  /** Allocate a pick target and read it back per frame. Default `false`. */
  picking?: boolean;
  /** Overlay an image so right-click offers the usual save entries. */
  /**
   * Give the canvas the context menu an ordinary image has. A string names
   * the saved file (`true` means `plot.png`); the name is a hint a browser
   * may ignore, since a `data:` URL carries no path.
   */
  saveOnRightClick?: boolean | string;
  /** Set `false` to skip fetching the bundled font when none is registered. */
  defaultFont?: boolean;
}

/** The size and dpi a document's writer recorded, if any. Advisory. */
export interface DocumentHints {
  width?: number;
  height?: number;
  dpi?: number;
}

/** A plot document bound to a canvas. */
export class PlotView {
  static create(
    canvas: HTMLCanvasElement,
    doc: Uint8Array | ArrayBuffer,
    opts?: PlotViewOptions,
  ): Promise<PlotView>;

  /** Draw now, dropping any frame already scheduled. */
  redraw(): void;

  /** Set the surface size explicitly, in CSS pixels. */
  resize(cssWidth: number, cssHeight: number, ratio?: number): void;

  setColorScheme(scheme: 'light' | 'dark' | 'auto'): void;

  /** The scheme last asked for — `'auto'` if it is following the OS. */
  colorScheme(): 'light' | 'dark' | 'auto';

  /** Whether the inverted theme is drawn. Resolves `'auto'`. */
  isDark(): boolean;

  /**
   * The row id under a point, in CSS pixels, or `undefined` for empty space.
   * Needs `picking: true`; may lag the visible frame slightly.
   */
  pickAt(cssX: number, cssY: number): number | undefined;

  hints(): DocumentHints;

  /** Detach observers and release the wasm-side handle. */
  free(): void;
}
