//! A desktop viewer for hephaestus plot documents.
//!
//! A `.hep` carries a plot's *configuration* rather than a picture of it, so
//! the thing to do with one on a desktop is re-solve it at the size of the
//! window it is shown in — resizing reflows, it does not scale. That is what
//! this is: a Tauri shell whose webview is chrome and canvas only, over a
//! native Rust core that reads documents and rasterizes them on the GPU.
//!
//! **There is no wasm here.** The two existing document consumers are browser
//! clients; this one links `hephaestus` natively, rasterizes through
//! `vello-hybrid`, and sends finished pixels to the webview. Because the
//! webview owns presentation there is no surface to share and no
//! `render_to_texture` path — [`Renderer::render_to_buffer`] normalizes to
//! straight alpha on both backends, which is byte-for-byte what canvas
//! `putImageData` wants.
//!
//! Two structural facts to know before reading anything else. A
//! [`PlotComposition`] is deliberately neither `Send` nor `Sync`, so one
//! thread owns every open document and everything else talks to it over a
//! channel ([`render`], [`channel`]). And **one window shows one document**,
//! which is what earns a native tab bar on macOS rather than a drawn one
//! ([`windows`]).
//!
//! [`Renderer::render_to_buffer`]: hephaestus::Renderer::render_to_buffer
//! [`PlotComposition`]: hephaestus::plot::PlotComposition

pub mod channel;
pub mod commands;
pub mod error;
pub mod render;
pub mod watch;
pub mod windows;

use std::path::{Path, PathBuf};

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Emitter, Manager, Runtime};

use channel::{Render, Request};
use windows::{PendingOpens, WindowMap};

/// Emitted to the focused window when a menu item it owns the meaning of is
/// chosen. The payload is the item's id.
///
/// Menu items are routed to a window rather than acted on here because most of
/// them are about *a document*, and with a window per document the focused
/// window is which one. Sending to every window would export every open plot
/// on one ⌘E.
pub const EVENT_MENU: &str = "menu-action";

/// Build the app and run it until it exits.
pub fn run() {
    // Windows and Linux hand an associated document over as an argument.
    // Collected before the builder so `setup` can open windows for it.
    let launch_paths = paths_from_args(std::env::args_os().skip(1), std::env::current_dir().ok());

    let app = tauri::Builder::default()
        // First, as this plugin requires: a second launch has to be
        // intercepted before anything else in the process gets going.
        .plugin(tauri_plugin_single_instance::init(|app, argv, cwd| {
            let paths = paths_from_args(argv.into_iter().skip(1), Some(PathBuf::from(cwd)));
            deliver(app, paths);
        }))
        .plugin(tauri_plugin_dialog::init())
        .manage(WindowMap::default())
        .manage(PendingOpens::default())
        .setup(move |app| {
            let handle = app.handle().clone();
            app.manage(Render::new(render::spawn(handle)));

            let menu = build_menu(app.handle())?;
            app.set_menu(menu)?;
            app.on_menu_event(|app, event| dispatch_menu(app, event.id().0.as_str()));

            // Windows are created here rather than declared in
            // `tauri.conf.json`: there is one per document and the count is
            // not known until the arguments have been read.
            //
            // One is opened **synchronously and unconditionally**, because
            // reading a document is not instant and the event loop exits the
            // moment it finds itself with no windows — so deferring this to
            // the task below is enough to make the app start and immediately
            // stop. It opens empty and `deliver` fills it, which is the same
            // path File ▸ Open from a blank window takes.
            windows::open_window(app.handle(), None);

            // Arguments first, then anything an Apple Event delivered before
            // this hook ran — which on a first launch by double-click is the
            // document the user actually asked for.
            let mut opening = launch_paths.clone();
            opening.extend(app.state::<PendingOpens>().drain());
            deliver(app.handle(), opening);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::attach,
            commands::open_dialog,
            commands::open_paths,
            commands::render_frame,
            commands::set_dark,
            commands::export_document,
        ])
        .build(tauri::generate_context!())
        .expect("the app could not be built from tauri.conf.json");

    app.run(|app, event| match event {
        // macOS only: Launch Services sends an Apple Event rather than an
        // argument. Unlike the previous shape this needs no buffering — the
        // window is created here and bound to its document before its own
        // scripts run, so there is nobody to race. Only reachable in a bundled
        // build.
        #[cfg(target_os = "macos")]
        tauri::RunEvent::Opened { urls } => {
            let paths = urls
                .iter()
                .filter_map(|url| url.to_file_path().ok())
                .collect::<Vec<_>>();
            deliver(app, paths);
        }
        tauri::RunEvent::ExitRequested { .. } => {
            if let Some(render) = app.try_state::<Render>() {
                let _ = render.send(Request::Shutdown);
            }
        }
        _ => {}
    });
}

/// Open `paths`, each in its own window.
///
/// Used for every route that does not come from a window — an argument, a
/// second launch, a double-click in Finder. An empty window is reused if there
/// is one, so launching with a document does not leave a blank window beside
/// it.
fn deliver<R: Runtime>(app: &AppHandle<R>, paths: Vec<PathBuf>) {
    if paths.is_empty() {
        return;
    }
    // The render thread is started in `setup`, and a double-click's Apple
    // Event arrives before that — so anything this early is held and opened by
    // the drain at the end of `setup`. `try_state` rather than `state`
    // throughout: the panicking form inside a spawned task is how the document
    // went missing without a word.
    if app.try_state::<Render>().is_none() {
        app.state::<PendingOpens>().queue(paths);
        return;
    }

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let Some(render) = app.try_state::<Render>() else {
            return;
        };
        let outcome = render.ask(|reply| Request::Open { paths, reply }).await;
        match outcome {
            Ok(outcome) => {
                let vacant = vacant_label(&app);
                windows::present(&app, vacant.as_deref(), &outcome.opened);
                for failure in &outcome.failed {
                    eprintln!("hephaestus-viewer: {}", failure.message);
                }
            }
            Err(error) => eprintln!("hephaestus-viewer: {error}"),
        }
    });
}

/// An open window showing nothing, if there is one.
fn vacant_label<R: Runtime>(app: &AppHandle<R>) -> Option<String> {
    let map = app.state::<WindowMap>();
    app.webview_windows()
        .keys()
        .find(|label| map.is_vacant(label))
        .cloned()
}

/// The window the user is looking at.
fn focused_label<R: Runtime>(app: &AppHandle<R>) -> Option<String> {
    app.webview_windows()
        .iter()
        .find(|(_, window)| window.is_focused().unwrap_or(false))
        .map(|(label, _)| label.clone())
}

/// Act on a menu item, or hand it to the focused window.
///
/// The two that open a window are Rust's to do; everything else is about the
/// document somebody is looking at, so it goes to that window alone.
fn dispatch_menu<R: Runtime>(app: &AppHandle<R>, id: &str) {
    if id == "new-window" {
        windows::open_window(app, None);
        return;
    }
    if let Some(label) = focused_label(app) {
        let _ = app.emit_to(label, EVENT_MENU, id);
    }
}

/// Absolute paths from a command line, resolved against `cwd`.
///
/// A file association passes a path that may be relative to wherever the
/// launcher happened to be, and a second launch's `cwd` is not this process's.
fn paths_from_args<A>(args: A, cwd: Option<PathBuf>) -> Vec<PathBuf>
where
    A: IntoIterator,
    A::Item: Into<std::ffi::OsString>,
{
    args.into_iter()
        .map(|arg| PathBuf::from(arg.into()))
        // Anything starting with a dash is a flag rather than a document;
        // macOS in particular passes `-psn_…` to a bundled app.
        .filter(|path| !path.to_string_lossy().starts_with('-'))
        .map(|path| resolve(path, cwd.as_deref()))
        .collect()
}

/// Make `path` absolute against `cwd` if it is not already.
fn resolve(path: PathBuf, cwd: Option<&Path>) -> PathBuf {
    if path.is_absolute() {
        return path;
    }
    match cwd {
        Some(cwd) => cwd.join(path),
        None => path,
    }
}

/// The application menu.
///
/// Every item the frontend owns carries an id it recognizes; the rest are the
/// platform's own, so Quit, Close Window and the Edit items behave the way the
/// OS provides them rather than being reimplemented. **⌘W is the platform's
/// Close Window** rather than an item of ours: with a window per document,
/// closing the window *is* closing the document, and letting AppKit do it is
/// what makes it behave correctly for a tab in a group.
fn build_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let new_window = MenuItem::with_id(app, "new-window", "New Window", true, Some("CmdOrCtrl+N"))?;
    let open = MenuItem::with_id(app, "open", "Open…", true, Some("CmdOrCtrl+O"))?;
    let export = MenuItem::with_id(app, "export", "Export…", true, Some("CmdOrCtrl+E"))?;
    let reload = MenuItem::with_id(app, "reload", "Reload", true, Some("CmdOrCtrl+R"))?;
    let toggle_dark = MenuItem::with_id(
        app,
        "toggle-dark",
        "Invert Theme",
        true,
        Some("CmdOrCtrl+Shift+I"),
    )?;

    let file = Submenu::with_items(
        app,
        "File",
        true,
        &[
            &new_window,
            &open,
            &PredefinedMenuItem::separator(app)?,
            &export,
            &PredefinedMenuItem::separator(app)?,
            &reload,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::close_window(app, None)?,
            #[cfg(not(target_os = "macos"))]
            &PredefinedMenuItem::quit(app, None)?,
        ],
    )?;

    let view = Submenu::with_items(app, "View", true, &[&toggle_dark])?;

    let edit = Submenu::with_items(
        app,
        "Edit",
        true,
        &[
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::select_all(app, None)?,
        ],
    )?;

    // AppKit adds its own tab items — Show All Tabs, Merge All Windows — to
    // the Window menu when a window carries a tabbing identifier.
    let window = Submenu::with_items(
        app,
        "Window",
        true,
        &[
            &PredefinedMenuItem::minimize(app, None)?,
            &PredefinedMenuItem::maximize(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::close_window(app, None)?,
        ],
    )?;

    Menu::with_items(
        app,
        &[
            #[cfg(target_os = "macos")]
            &Submenu::with_items(
                app,
                app.package_info().name.clone(),
                true,
                &[
                    &PredefinedMenuItem::about(app, None, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::services(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::hide(app, None)?,
                    &PredefinedMenuItem::hide_others(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::quit(app, None)?,
                ],
            )?,
            &file,
            &edit,
            &view,
            &window,
        ],
    )
}
