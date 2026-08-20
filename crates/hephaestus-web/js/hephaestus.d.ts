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
 * A hard gate, not a preference: the backend rasterises through compute
 * shaders, which WebGL2 cannot run, so there is no fallback path. Serve a
 * static image when this is `false`.
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
 * under. Throws on WOFF2 / WOFF, and on bytes holding no recognisable face.
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

/** Fetch a TTF/OTF/TTC/OTC and register it. WOFF2 is rejected. */
export function registerFontFromUrl(
  url: string,
  opts?: { genericFor?: string; key?: string },
): Promise<string[]>;

/**
 * Register a Google Fonts family through the Developer API v1.
 *
 * Needs a key: the CSS2 API serves TTF only to clients sending no
 * `User-Agent`, which `fetch` cannot do, so a page always gets WOFF2 there.
 */
export function registerGoogleFont(
  family: string,
  opts: { apiKey: string; variants?: string[]; genericFor?: string },
): Promise<string[]>;

export interface PlotViewOptions {
  /** Theme to draw with. `'auto'` follows `prefers-color-scheme`. */
  colorScheme?: 'light' | 'dark' | 'auto';
  /** Track the canvas's CSS box with a ResizeObserver. Default `true`. */
  autoResize?: boolean;
  /** Allocate a pick target and read it back per frame. Default `false`. */
  picking?: boolean;
  /** Overlay an image so right-click offers the usual save entries. */
  saveOnRightClick?: boolean;
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
