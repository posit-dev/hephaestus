# Trying this on Windows

None of the Windows code has ever been built, registered or run. It
type-checks against `x86_64-pc-windows-msvc` from a Mac, which catches wrong
COM signatures and missing `windows` crate features and nothing else.

**You are the first person to run it.** These steps are ordered so each one
rules out everything below it — please work through them in order and stop at
the first that fails, because a failure at step 3 makes anything you observe at
step 6 meaningless.

There are two separate things to test and they are independent: **the app**
(steps 1–3) and **the Explorer shell extensions** (steps 4–8). If you only have
time for one, step 3 is the most valuable, because it is the only *measurement*
nobody has been able to take.

---

## 0. Prerequisites

- Windows 10 or 11, x64.
- **Visual Studio Build Tools** with the "Desktop development with C++"
  workload. Rust needs the MSVC linker, and this is the step people skip.
- Rust (`x86_64-pc-windows-msvc` is the default toolchain on Windows).
- `cargo install tauri-cli --version '^2'`
- **WebView2 runtime.** Present on Windows 11 and most Windows 10; the app
  will refuse to start without it.
- Node 18+, only for `node ui/verify.mjs`.
- An **elevated** PowerShell for steps 5 onward. `regsvr32` writes to
  `HKEY_CLASSES_ROOT` and `HKLM`.
- [DebugView](https://learn.microsoft.com/sysinternals/downloads/debugview)
  from Sysinternals, for step 7. Nothing else can see inside a shell
  extension.

A test document first — there is none in the repo:

```powershell
cd <repo>
cargo run --example document_save --features document-write
# writes examples\document.hep
```

---

## 1. Does the app build and run?

```powershell
cd crates\hephaestus-explorer
cargo build  --release --target x86_64-pc-windows-msvc
```

```powershell
cd crates\hephaestus-viewer
cargo run --release -- ..\..\examples\document.hep
```

Expected: a window with a plot in it, titled `document.hep`.

- **Nothing appears / it exits immediately.** Check WebView2 is installed. Also
  check for a leftover process — `Get-Process hephaestus-viewer` — because the
  single-instance plugin makes a second launch forward its arguments to the
  first and exit `0` silently, which looks exactly like a crash.
- **A window with an empty "No document open" state.** The argument did not
  arrive. Report it; that path is the one that had a bug on macOS.

Worth a minute each: drag another `.hep` onto the window; **Ctrl+O**,
**Ctrl+E**, **Ctrl+W**, **Ctrl+R**; drag the window edge and watch whether the
plot re-lays-out rather than stretching.

<!-- 
Ctrl+O, Ctrl+E, Ctrl+W, Ctrl+R  shortcuts doesn't work, but menu options seem to work fine.
Resizing works well, invert button switches between light/dark mode.
Plot opened up in dark mode, matching system settings
-->

> On Windows a second document opens a **second window**, not a tab. That is
> expected — window tabs are a macOS feature, and a drawn tab strip for Windows
> is a known gap.

<!-- 
At this point files aren't associated with the executable yet, so there
isn't really a convenient way to open a second document yet.
Terminal is blocked  untill window is closed.
Might be instruction shortcoming rather than actual shortcoming.
-->

---

## 2. Does the frontend seam hold?

```powershell
node ui\verify.mjs
```

13 checks, all should pass. This is platform-independent, so a failure here
means something drifted rather than something Windows-specific.

<!-- 
All 13 checks pass
-->

---

## 3. ⭐ Measure the IPC transport — the one number that is a guess

This is the most valuable thing on the page. Every frame crosses from Rust to
the webview as raw bytes, and **the Windows cost is taken from an upstream
benchmark (~200 ms per 10 MB, tauri#11915) that nobody here has reproduced.**
The whole PNG-encoding fallback exists because of that number. If it is wrong,
a default should change.

<!-- 
Per earlier instructions we're still in crates\hephaestus-viewer
First cargo run opens .hep as before
We don't need --release versions, they don't print the numbers. Only debug versions print the numbers.
-->

```powershell
# the default on Windows: PNG-encoded frames
$env:HEPHAESTUS_VIEWER_TRACE=1
cargo run -- ..\..\examples\document.hep

# then the same thing with raw pixels, in a fresh shell
$env:HEPHAESTUS_VIEWER_TRACE=1
$env:HEPHAESTUS_VIEWER_ENCODING="raw"
cargo run  -- ..\..\examples\document.hep
```

<!-- 
First situation:
frame tab=1 seq=1 1100x702 @96dpi PngFast 114539 bytes in 908.3ms
frame tab=1 seq=2 1100x701 @96dpi draft PngFast 49550 bytes in 196.4ms
frame tab=1 seq=3 999x580 @96dpi draft PngFast 44418 bytes in 172.6ms
frame tab=1 seq=4 884x494 @96dpi draft PngFast 39729 bytes in 154.8ms
frame tab=1 seq=5 883x493 @96dpi draft PngFast 39729 bytes in 79.5ms
frame tab=1 seq=6 883x493 @96dpi PngFast 91107 bytes in 286.0ms
frame tab=1 seq=7 2560x1311 @96dpi draft PngFast 82706 bytes in 473.2ms
frame tab=1 seq=8 2560x1311 @96dpi PngFast 209331 bytes in 1409.8ms

Second situation:
frame tab=1 seq=1 1100x702 @96dpi RawRgba 3088832 bytes in 651.3ms
frame tab=1 seq=2 1100x701 @96dpi draft RawRgba 772232 bytes in 122.6ms
frame tab=1 seq=3 982x590 @96dpi draft RawRgba 579412 bytes in 116.2ms
frame tab=1 seq=4 782x422 @96dpi draft RawRgba 330036 bytes in 109.4ms
frame tab=1 seq=5 781x422 @96dpi draft RawRgba 330036 bytes in 36.3ms
frame tab=1 seq=6 781x422 @96dpi RawRgba 1318360 bytes in 132.5ms
frame tab=1 seq=7 782x422 @96dpi draft RawRgba 330036 bytes in 113.7ms
frame tab=1 seq=8 942x426 @96dpi draft RawRgba 401324 bytes in 115.4ms
frame tab=1 seq=9 1577x410 @96dpi draft RawRgba 647012 bytes in 117.1ms
frame tab=1 seq=10 1803x369 @96dpi draft RawRgba 667512 bytes in 111.9ms
frame tab=1 seq=11 1822x363 @96dpi draft RawRgba 663240 bytes in 109.6ms
frame tab=1 seq=12 1822x363 @96dpi RawRgba 2645576 bytes in 139.8ms
frame tab=1 seq=13 1821x363 @96dpi draft RawRgba 663240 bytes in 109.7ms
frame tab=1 seq=14 1821x363 @96dpi RawRgba 2644124 bytes in 139.2ms
frame tab=1 seq=15 2560x1311 @96dpi draft RawRgba 3358752 bytes in 153.5ms
frame tab=1 seq=16 2560x1311 @96dpi RawRgba 13424672 bytes in 250.8ms
-->

Each frame prints a line like:

```
frame tab=1 seq=1 2200x1308 @192dpi PngFast 282479 bytes in 17.0ms
```

**Resize the window around for a few seconds in each mode**, then report:

1. The steady-state `in …ms` figures for each encoding (ignore the first
   frame — that is GPU setup).
2. Whether resizing *feels* different between them. This matters more than the
   numbers: the trace measures render plus encode, **not** the IPC transfer,
   so a large gap between "the numbers look the same" and "raw feels worse" is
   itself the finding.

For reference, measured on an M-series Mac at 2200×1308: raw 11.5 MB in
~10.6 ms, PNG 282 kB in ~17.0 ms — a ~50× compression ratio.

---

## 4. Does the shell-extension DLL build?

<!-- 
I had to build this before the viewer due to some build script dependency.
But yes, it builds
-->

```powershell
cd crates\hephaestus-explorer
cargo build --release
```

This is the first time it has been **linked**, so this is a real risk step.

Then check the DLL actually exports what COM looks for — a `cdylib` should
export `#[no_mangle] pub extern "system"` functions, but verify rather than
assume:

```powershell
dumpbin /EXPORTS target\release\hephaestus_explorer.dll
```

You should see `DllGetClassObject`, `DllCanUnloadNow`, `DllRegisterServer`
and `DllUnregisterServer`. **If any are missing, stop** — nothing later can
work, and the fix is a `.def` file or `#[used]`/`dllexport` handling.

<!-- 
dumpbin wasn't on PATH, but yes these 4 Dll thingies are reported
-->

---

## 5. Does rendering work on this machine at all?

Before blaming the shell, prove the renderer works *outside* a surrogate
process. This isolates "the GPU works here" from "the GPU works inside
Explorer's sandbox", which is the failure I most expect:

```powershell
cd crates\hephaestus-thumbnailer
cargo run --release -- -s 512 -i ..\..\examples\document.hep -o $env:TEMP\t.png
```

Open `%TEMP%\t.png`. It should be the plot, 512 px on its longer edge.


<!-- 
It created a 512px x 239px t.png
-->

- **This fails** → the problem is `vello-hybrid` on this machine (no D3D12
  adapter?), not the shell extensions. Report the error; everything below will
  fail too.
- **This works but step 8 does not** → the GPU is the surrogate problem, which
  is the case with a known fix (`Windows.Data.Pdf`, see `CLAUDE.md`).

---

## 6. Register the extensions

**Elevated** PowerShell:

```powershell
cd crates\hephaestus-explorer
regsvr32 target\release\hephaestus_explorer.dll
```

<!-- 
Succeeds when 'Run as administrator'-opening powershell
-->


A success dialog means the DLL loaded and `DllRegisterServer` returned `S_OK`.
An error dialog names the failure.

Confirm what landed:

```powershell
reg query "HKCR\.hep\ShellEx" /s
reg query "HKCR\CLSID\{7E1B4C2A-9D3F-4A61-8B52-6C0E9A47D310}" /s
reg query "HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\PreviewHandlers" /v "{7E1B4C2B-9D3F-4A61-8B52-6C0E9A47D310}"
```

Then make Explorer notice. It caches handlers hard and this is the single most
common reason a correct extension appears to do nothing:

```powershell
taskkill /f /im explorer.exe; Start-Process explorer.exe
Remove-Item "$env:LocalAppData\Microsoft\Windows\Explorer\thumbcache_*.db" -Force -ErrorAction SilentlyContinue
```

<!-- 
When using medium-sized icons or larger, the document.hep file in the examples folder shows the plot as icon.
This behaviour is the same as the .png files.
Unlike the .png files, it does not show up in the 'Preview pane' in windows explorer if that is turned on.
I was today years old when I learned about the Preview pane, so I wouldn't worry about it 
-->

---

## 7. Turn on the diagnostics before testing

The extensions run inside `dllhost.exe` and `prevhost.exe`, so there is no
console. Build with the diagnostic channel and watch it:

```powershell
cargo build --release --features diagnostics
regsvr32 /s /u target\release\hephaestus_explorer.dll
regsvr32 target\release\hephaestus_explorer.dll
taskkill /f /im explorer.exe; Start-Process explorer.exe
```

Run **DebugView elevated**, and turn on **Capture → Capture Global Win32**
(without it you see nothing from a surrogate). Lines are prefixed
`[hephaestus]`.

You should see `DllGetClassObject`, then `initialized with N bytes`, then a
render line. Where it stops tells you which step failed.

---

## 8. The actual tests

Put `examples\document.hep` somewhere ordinary like `Documents`.

**Thumbnail** — switch the folder to **Large icons** or **Extra large icons**.
Expected: the plot as the icon instead of a generic document page.

**Preview pane** — press **Alt+P**, then select the file. Expected: the plot,
and **it should re-lay-out as you drag the pane's divider** — axis labels
repositioning and tick counts changing, not a picture being stretched. This is
the only preview in the project that reflows, so it is worth looking at
closely.

<!-- 
I do see [hephaestus]-prefixed stuff in the debugviewer, like this:

[hephaestus] DllGetClassObject: creating a factory for Thumbnail 
[hephaestus] DllGetClassObject: creating a factory for Thumbnail 
[hephaestus] thumbnail: initialized with 10553 bytes 
[hephaestus] thumbnail: rendering at 1280px 
[hephaestus] thumbnail: 1280x597 
[hephaestus] thumbnail: initialized with 10553 bytes 
[hephaestus] thumbnail: rendering at 1280px 
[hephaestus] thumbnail: 1280x597 

Thumbnails seem to work as expected. 
Preview pane says 'This file can't be previewed', so I think that doesn't work.
-->

While you are there: does a fast drag of the divider feel laggy? There is
deliberately no debounce on re-render, and whether one is needed is an open
question.

**Diagnostics to expect if it silently does nothing:**

| symptom | what it means |
| --- | --- |
| `dllhost.exe` / `prevhost.exe` never appears in Task Manager | the shell is not loading the class — registration or CLSID problem |
| surrogate appears, DebugView silent | the DLL loaded but our class was not instantiated |
| `initialized with N bytes` then nothing | rendering failed inside the surrogate — almost certainly the GPU |
| `no GPU adapter` | confirms it; see `CLAUDE.md` for the `Windows.Data.Pdf` alternative |

<!-- 
I'm not seeing `dllhost.exe` in task manager, but do see `prevhost.exe`
-->

---

## 9. The installer, last

Only once the pieces work by hand:

```powershell
cd crates\hephaestus-viewer
.\package-windows.ps1
```
Then install the NSIS package from `target\release\bundle\nsis\`, and check
the extensions registered themselves without `regsvr32` being run by hand —
`packaging\installer-hooks.nsh` should have done it. Uninstall and confirm they
are removed.

Note the installer will not be Authenticode-signed, so SmartScreen will warn.
That is expected and unrelated.

<!-- 
Had to run line below:
Set-ExecutionPolicy -Scope CurrentUser RemoteSigned
-->

---

## What to send back

Even a partial run is useful. Most valuable first:

1. **The step 3 numbers**, and how each encoding felt. This is a real
   measurement replacing a guess.
2. **Where you stopped**, and the exact error or DebugView tail.
3. Whether the preview pane reflowed on resize — that is the design claim the
   whole `IPreviewHandler` exists to make good on.
4. Anything about the *app* (steps 1–2) that felt wrong, especially around
   window management, since Windows gets separate windows where macOS gets
   tabs.

<!-- 
Preview pane didn't work, but else looks good
-->

Do not spend time trying to fix the shell extensions. Report where it stops;
the failure modes are enumerated in `CLAUDE.md` under "What to check first"
and most of them have a known remedy.
