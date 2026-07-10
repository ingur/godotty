//! Keeps the editor in sync with external edits to the project.
//!
//! Godot reconciles the filesystem, open scripts, scenes, shaders, and
//! extensions only on `NOTIFICATION_APPLICATION_FOCUS_IN`. With a terminal
//! living inside the editor, focus never leaves, so edits from neovim or a
//! coding agent stay invisible until an alt-tab. A `notify` watch over the
//! project reloads everything that is safe to reload silently and live.
//!
//! Conflicts (unsaved editor state whose file also changed on disk) need a
//! human decision, so they surface the editor's own disk-change prompts
//! live, exactly as an alt-tab would: the script editor's prompt for
//! scripts, the EditorNode one for scenes and project.godot. Both prompts
//! are exclusive modals sharing one slot, and Cancel reconciles nothing, so
//! presentations are serialized (one at a time, never over another modal).
//! A prompt fires once per external write: dismissing it and then editing
//! the file again on disk re-prompts, so accumulated external revisions are
//! never silently overwritten, but an idle unresolved conflict never loops.
//!
//! The reload APIs assume the editor is the focus destination (they run on
//! alt-tab), so they grab keyboard focus into whatever they refresh. A
//! background sync must not, so the focus owner is captured before each
//! sync and restored when it ends, leaving any prompt the sync raised
//! untouched.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::time::SystemTime;

use godot::classes::notify::NodeNotification;
use godot::classes::{
    ConfigFile, Control, EditorInterface, GDExtensionManager, INode, ProjectSettings, TextEdit,
    Window,
};
use godot::prelude::*;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher, recommended_watcher};

/// Coalesce a burst of writes (a save, an agent touching many files) into
/// one sync once the writes stop for this long.
const SETTLE_SECS: f64 = 0.2;

/// Re-check held work while a conflict prompt waits on the user.
const RETRY_SECS: f64 = 0.5;

const AUTORELOAD_SETTING: &str = "text_editor/behavior/files/auto_reload_scripts_on_external_change";

/// Owns the watcher and polls it from its own `process`. The reloads it
/// drives make the editor call back into `EditorPlugin` methods (e.g.
/// `get_plugin_name` during a main-screen refresh); running them here keeps
/// `TerminalPanel` unbound so those callbacks never hit a re-entrant borrow.
#[derive(GodotClass)]
#[class(tool, base = Node)]
pub struct SyncNode {
    base: Base<Node>,
    sync: Option<ExternalSync>,
}

#[godot_api]
impl INode for SyncNode {
    fn init(base: Base<Node>) -> Self {
        SyncNode { base, sync: None }
    }

    fn ready(&mut self) {
        self.sync = ExternalSync::start();
        if self.sync.is_none() {
            godot_warn!("[godotty] file watcher unavailable; external edits sync on focus only");
        }
        self.base_mut().set_process(true);
    }

    fn process(&mut self, delta: f64) {
        if let Some(sync) = self.sync.as_mut() {
            sync.poll(delta);
        }
    }
}

impl SyncNode {
    pub fn note_scene_saved(&mut self, path: &str) {
        if let Some(sync) = self.sync.as_mut() {
            sync.note_scene_saved(path);
        }
    }
}

/// A recursive watch over the project directory, drained on the main thread
/// because everything it drives is main-thread editor API.
struct ExternalSync {
    // Dropping the watcher stops the OS watch and its thread.
    _watcher: RecommendedWatcher,
    events: Receiver<notify::Result<Event>>,
    /// Project root as the editor spells it, and its canonical form for
    /// remapping the resolved paths the OS watcher reports.
    root: PathBuf,
    canonical_root: PathBuf,
    changed: HashSet<PathBuf>,
    settle: f64,
    retry: f64,
    /// Disk mtime last acted on per path; duplicate watch events for the
    /// same write must not reload a scene twice.
    delivered: HashMap<PathBuf, Option<SystemTime>>,
    /// Disk mtime right after the editor itself saved a scene. Reloading
    /// that would only rebuild the tab and clear its undo history.
    scene_saves: HashMap<PathBuf, Option<SystemTime>>,
    /// Changes not yet flushed to the script editor; its refresh is
    /// all-or-nothing across tabs, so flushes wait out open conflicts.
    script_pending: HashSet<PathBuf>,
    /// Script conflicts already prompted: unsaved tab, changed disk. Held
    /// until the tab is saved, reloaded, or closed, so each prompts once.
    script_conflicts: HashSet<PathBuf>,
    /// Scene conflicts waiting for a free modal slot to be prompted.
    scene_dialog_pending: HashSet<PathBuf>,
    /// Scene conflicts already prompted, held like script conflicts.
    scene_conflicts: HashSet<PathBuf>,
    /// project.godot changed; the EditorNode prompt owns its reload and
    /// filters out the editor's own saves via its last-saved bookkeeping.
    project_pending: bool,
    /// Shader files changed this sync, handed to the shader editors once.
    shader_pending: HashSet<PathBuf>,
    /// Library path to owning .gdextension, for direct hot reload.
    extensions: HashMap<PathBuf, GString>,
    /// A watcher error was already reported; do not repeat it every event.
    watch_error_warned: bool,
}

impl ExternalSync {
    fn start() -> Option<ExternalSync> {
        let root = project_root()?;
        let canonical_root = root.canonicalize().unwrap_or_else(|_| root.clone());
        let (tx, events) = channel();
        let mut watcher = recommended_watcher(tx).ok()?;
        watcher.watch(&canonical_root, RecursiveMode::Recursive).ok()?;
        Some(ExternalSync {
            _watcher: watcher,
            events,
            root,
            canonical_root,
            changed: HashSet::new(),
            settle: 0.0,
            retry: 0.0,
            delivered: HashMap::new(),
            scene_saves: HashMap::new(),
            script_pending: HashSet::new(),
            script_conflicts: HashSet::new(),
            scene_dialog_pending: HashSet::new(),
            scene_conflicts: HashSet::new(),
            project_pending: false,
            shader_pending: HashSet::new(),
            extensions: extension_map(),
            watch_error_warned: false,
        })
    }

    /// Called from the plugin's scene_saved signal.
    fn note_scene_saved(&mut self, path: &str) {
        let path = globalize(path);
        let mtime = mtime(&path);
        self.scene_saves.insert(path, mtime);
    }

    /// Drain watch events and, once a burst settles, sync the editor. Held
    /// conflict work is retried on a slower tick until the user resolves it.
    fn poll(&mut self, delta: f64) {
        while let Ok(event) = self.events.try_recv() {
            let event = match event {
                Ok(event) => event,
                // An error usually means the backend dropped events (queue
                // overflow); our mtime map may now be stale until the next
                // real change or focus-in re-reconciles.
                Err(_) => {
                    if !self.watch_error_warned {
                        godot_warn!("[godotty] file watcher error; some edits may sync late");
                        self.watch_error_warned = true;
                    }
                    continue;
                }
            };
            // Access events (opens, reads) are dropped so the reads our own
            // reloads perform cannot feed back into the watcher.
            if event.kind.is_access() {
                continue;
            }
            for path in event.paths {
                if watched(&path) {
                    self.changed.insert(self.rooted(path));
                    self.settle = SETTLE_SECS;
                }
            }
        }
        if !self.changed.is_empty() {
            self.settle -= delta;
            if self.settle > 0.0 {
                return;
            }
            let batch = std::mem::take(&mut self.changed);
            self.sync(batch);
        } else if self.has_held_work() {
            self.retry -= delta;
            if self.retry > 0.0 {
                return;
            }
            self.retry = RETRY_SECS;
            self.sync(HashSet::new());
        }
    }

    /// Conflicts resolve through user actions that produce no watch events
    /// (a prompt answer, a save, an undo), so held work re-checks on a slow
    /// tick.
    fn has_held_work(&self) -> bool {
        !self.script_pending.is_empty()
            || !self.script_conflicts.is_empty()
            || !self.scene_dialog_pending.is_empty()
            || !self.scene_conflicts.is_empty()
            || self.project_pending
    }

    fn sync(&mut self, batch: HashSet<PathBuf>) {
        // The reloads below run synchronously and some grab focus; capture
        // the owner now and restore it once they return, before any prompt
        // they queued (those are deferred) legitimately takes over.
        let focus_before = focus_owner();
        let fresh = self.deliver(batch);
        if !fresh.is_empty() {
            // The exact rescan the focus path runs; it queues itself when a
            // scan is already going.
            if let Some(mut filesystem) = EditorInterface::singleton().get_resource_filesystem() {
                filesystem.scan_sources();
            }
            self.reload_extensions(&fresh);
            self.reload_scenes(&fresh);
            self.queue_shaders(&fresh);
            self.script_pending.extend(fresh);
        }
        self.prune_scene_conflicts();
        // One prompt per tick at most; the retry tick presents whatever had
        // to wait, after the open prompt is answered.
        let presented = self.flush_scripts();
        if !presented {
            self.present_scene_dialog();
        }
        self.flush_shaders();
        restore_focus(focus_before);
    }

    /// Paths whose disk state moved since we last acted on them.
    fn deliver(&mut self, batch: HashSet<PathBuf>) -> Vec<PathBuf> {
        let mut fresh = Vec::new();
        for path in batch {
            let mtime = mtime(&path);
            if self.delivered.get(&path) != Some(&mtime) {
                self.delivered.insert(path.clone(), mtime);
                fresh.push(path);
            }
        }
        // Agents and editors churn out temp and backup files; drop entries
        // for paths that no longer exist so the map cannot grow unbounded (a
        // reappearing path is simply treated as fresh again).
        if self.delivered.len() > 2048 {
            self.delivered.retain(|path, _| path.exists());
        }
        fresh
    }

    /// Hot reload rebuilt GDExtension libraries. Our own library is never
    /// reloaded: tearing down the extension this terminal runs from
    /// mid-frame crashes the editor, so godotty rebuilds are picked up on
    /// the next editor restart.
    fn reload_extensions(&mut self, fresh: &[PathBuf]) {
        for path in fresh {
            if is_own(path) {
                continue;
            }
            let config_changed = path.extension().is_some_and(|e| e == "gdextension");
            let Some(extension) = self
                .extensions
                .get(path)
                .cloned()
                .or_else(|| config_changed.then(|| localize(path)).flatten())
            else {
                continue;
            };
            let mut manager = GDExtensionManager::singleton();
            if !manager.is_extension_loaded(&extension) {
                continue;
            }
            // Deferred: reloading can unload this very library, so our own
            // stack must be clear when it happens.
            manager.call_deferred("reload_extension", &[extension.to_variant()]);
            if config_changed {
                self.extensions = extension_map();
            }
        }
    }

    /// Open, unmodified scenes reload silently. Stock focus-in prompts even
    /// for clean scenes; reloading them in place is the improvement this
    /// feature adds. Conflicted scenes and project.godot queue for the
    /// EditorNode prompt, which owns the only reload paths for both.
    fn reload_scenes(&mut self, fresh: &[PathBuf]) {
        let editor = EditorInterface::singleton();
        let open = globalized(&editor.get_open_scenes());
        let unsaved = globalized(&editor.get_unsaved_scenes());
        let project = self.root.join("project.godot");
        for path in fresh {
            if *path == project {
                self.project_pending = true;
                continue;
            }
            if !open.contains(path) || !path.exists() {
                continue;
            }
            if self.scene_saves.get(path) == Some(&mtime(path)) {
                continue;
            }
            self.scene_saves.remove(path);
            if unsaved.contains(path) {
                self.scene_dialog_pending.insert(path.clone());
            } else {
                reload_scene(path);
            }
        }
    }

    /// Drop resolved conflicts; reload any left clean but stale by an undo.
    fn prune_scene_conflicts(&mut self) {
        if self.scene_conflicts.is_empty() {
            return;
        }
        let editor = EditorInterface::singleton();
        let open = globalized(&editor.get_open_scenes());
        let unsaved = globalized(&editor.get_unsaved_scenes());
        let conflicts = std::mem::take(&mut self.scene_conflicts);
        for path in conflicts {
            if open.contains(&path) && unsaved.contains(&path) {
                self.scene_conflicts.insert(path);
            } else if open.contains(&path)
                && path.exists()
                && self.delivered.get(&path) == Some(&mtime(&path))
                && self.scene_saves.get(&path) != Some(&mtime(&path))
            {
                reload_scene(&path);
            }
        }
    }

    /// The script editor's refresh is all-or-nothing: it silently reloads
    /// every clean changed tab, but prompts if any changed tab is unsaved.
    /// Each fresh external write to an unsaved tab presents that prompt, when
    /// no other modal is up (a prior unanswered prompt holds the rest). With
    /// no conflicts at all the flush is provably silent. Returns whether a
    /// prompt was presented.
    fn flush_scripts(&mut self) -> bool {
        if self.script_pending.is_empty() && self.script_conflicts.is_empty() {
            return false;
        }
        if !autoreload_enabled() {
            // With autoreload off the editor prompts even for clean tabs;
            // the user chose prompts everywhere, which belongs to the real
            // focus-in, not a background sync.
            self.script_pending.clear();
            self.script_conflicts.clear();
            return false;
        }
        let Some(mut script_editor) = EditorInterface::singleton().get_script_editor() else {
            return false;
        };
        let unsaved = globalized(&script_editor.get_unsaved_files());
        let mut dissolved = false;
        self.script_conflicts.retain(|path| {
            let outstanding = unsaved.contains(path);
            dissolved |= !outstanding;
            outstanding
        });
        // A conflict is an unsaved tab whose file still holds the change we
        // delivered. Presence in script_pending means the file just changed
        // on disk, so an already-tracked conflict here is a new write and
        // re-prompts rather than being skipped.
        let new_conflicts: Vec<PathBuf> = self
            .script_pending
            .iter()
            .filter(|path| {
                unsaved.contains(*path) && self.delivered.get(*path) == Some(&mtime(path))
            })
            .cloned()
            .collect();
        if !new_conflicts.is_empty() {
            if modal_open() {
                return false;
            }
            self.script_conflicts.extend(new_conflicts);
            self.script_pending.clear();
            script_editor.reload_open_files();
            return true;
        }
        if !self.script_conflicts.is_empty() {
            return false;
        }
        if self.script_pending.is_empty() && !dissolved {
            return false;
        }
        // reload_open_files can deferred-pop a dialog; never over an open modal.
        if modal_open() {
            return false;
        }
        self.script_pending.clear();
        script_editor.reload_open_files();
        true
    }

    /// Scene conflicts and project.godot changes surface the EditorNode
    /// disk-change prompt by handing it the focus-in notification its
    /// handler was written for. Presented once per external write, never
    /// over another modal; each entry here came from a fresh write, so it
    /// re-arms the prompt. EditorNode's own bookkeeping drops anything
    /// already current, so a nudge for the editor's own save shows nothing.
    fn present_scene_dialog(&mut self) {
        if self.scene_dialog_pending.is_empty() && !self.project_pending {
            return;
        }
        if modal_open() {
            return;
        }
        let editor = EditorInterface::singleton();
        let open = globalized(&editor.get_open_scenes());
        let unsaved = globalized(&editor.get_unsaved_scenes());
        let mut fresh_info = std::mem::take(&mut self.project_pending);
        for path in std::mem::take(&mut self.scene_dialog_pending) {
            if open.contains(&path)
                && unsaved.contains(&path)
                && self.delivered.get(&path) == Some(&mtime(&path))
            {
                self.scene_conflicts.insert(path);
                fresh_info = true;
            } else if open.contains(&path)
                && path.exists()
                && self.delivered.get(&path) == Some(&mtime(&path))
                && self.scene_saves.get(&path) != Some(&mtime(&path))
            {
                // Dissolved while waiting for the slot: clean but stale.
                reload_scene(&path);
            }
        }
        if !fresh_info {
            return;
        }
        match editor_node() {
            Some(mut node) => node.notify(NodeNotification::APPLICATION_FOCUS_IN),
            None => godot_warn!(
                "[godotty] EditorNode not found; refocus the editor to resolve external changes"
            ),
        }
    }

    fn queue_shaders(&mut self, fresh: &[PathBuf]) {
        for path in fresh {
            let shader = path
                .extension()
                .is_some_and(|e| e == "gdshader" || e == "gdshaderinc");
            if shader && path.exists() {
                self.shader_pending.insert(path.clone());
            }
        }
    }

    /// Shader editors reload on the focus-in notification, but unlike
    /// scripts they replace even unsaved buffers. Only editors whose buffer
    /// is saved are notified; each no-ops unless its own file changed. An
    /// editor with unsaved edits is left alone (its edits kept, its disk
    /// change ignored until it is saved), so one unsaved shader never holds
    /// up the others.
    fn flush_shaders(&mut self) {
        if self.shader_pending.is_empty() {
            return;
        }
        self.shader_pending.clear();
        if !autoreload_enabled() {
            // Their editors would prompt for any change; that belongs to the
            // real focus-in.
            return;
        }
        let Some(base) = EditorInterface::singleton().get_base_control() else {
            return;
        };
        let editors = base
            .find_children_ex("*")
            .type_("TextShaderEditor")
            .owned(false)
            .done();
        for mut editor in editors.iter_shared() {
            if !shader_buffer_unsaved(&editor) {
                editor.notify(NodeNotification::APPLICATION_FOCUS_IN);
            }
        }
    }

    /// Watcher backends report resolved paths; map them back under the root
    /// spelling the editor uses so set lookups line up.
    fn rooted(&self, path: PathBuf) -> PathBuf {
        match path.strip_prefix(&self.canonical_root) {
            Ok(rest) => self.root.join(rest),
            Err(_) => path,
        }
    }
}

/// Our own extension: its library is `lib<crate>.<platform>` and its config
/// is `<crate>.gdextension`, so both carry the crate name.
fn is_own(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.contains(env!("CARGO_PKG_NAME")))
}

/// Skip Godot's own churn: `.godot` is rewritten by every scan, `.import`
/// and `.uid` sidecars by scans and reimports, and `.git` by the very agent
/// whose edits we are tracking.
fn watched(path: &Path) -> bool {
    if path
        .extension()
        .is_some_and(|ext| ext == "import" || ext == "uid")
    {
        return false;
    }
    !path
        .components()
        .any(|c| matches!(c.as_os_str().to_str(), Some(".godot" | ".git")))
}

/// EditorNode is not exposed, but it is a Node above the exposed base
/// control; walking up survives the editor restructuring its layout.
fn editor_node() -> Option<Gd<Node>> {
    let mut node: Gd<Node> = EditorInterface::singleton().get_base_control()?.upcast();
    loop {
        if node.get_class() == "EditorNode" {
            return Some(node);
        }
        node = node.get_parent()?;
    }
}

/// A prompt is up somewhere in the editor. The editor tracks one exclusive
/// child per window, so presenting another now would corrupt that slot; we
/// wait. Key focus is per-viewport, so the focused control never reports a
/// dialog; scan the tree for a visible exclusive window instead. Floating
/// tool windows are not exclusive and do not count.
fn modal_open() -> bool {
    let Some(node) = editor_node() else {
        return false;
    };
    node.find_children_ex("*")
        .type_("Window")
        .owned(false)
        .done()
        .iter_shared()
        .filter_map(|node| node.try_cast::<Window>().ok())
        .any(|window| window.is_visible() && window.is_exclusive())
}

/// The editor's own unsaved test for a shader editor: its code buffer has
/// edits past the last tagged save. A buffer we cannot find reads as
/// unsaved, never risking a discard.
fn shader_buffer_unsaved(editor: &Gd<Node>) -> bool {
    let edits = editor
        .find_children_ex("*")
        .type_("CodeEdit")
        .owned(false)
        .done();
    let Some(edit) = edits.iter_shared().next() else {
        return true;
    };
    let Ok(edit) = edit.try_cast::<TextEdit>() else {
        return true;
    };
    edit.get_version() != edit.get_saved_version()
}

fn autoreload_enabled() -> bool {
    EditorInterface::singleton()
        .get_editor_settings()
        .map(|s| s.get_setting(AUTORELOAD_SETTING))
        .and_then(|v| v.try_to::<bool>().ok())
        .unwrap_or(true)
}

fn focus_owner() -> Option<Gd<Control>> {
    EditorInterface::singleton()
        .get_base_control()?
        .get_viewport()?
        .gui_get_focus_owner()
}

/// Put focus back where the user left it. A synchronous reload may have
/// grabbed it into whatever it refreshed; a prompt the reload queued is
/// deferred and lives in its own viewport, so it is not this window's focus
/// owner and is never disturbed. If the owner did not change, or the user
/// moved it, leave it.
fn restore_focus(before: Option<Gd<Control>>) {
    let Some(mut before) = before else { return };
    if !before.is_instance_valid() {
        return;
    }
    let Some(current) = focus_owner() else { return };
    if current != before {
        before.grab_focus();
    }
}

fn reload_scene(path: &Path) {
    if let Some(local) = localize(path) {
        EditorInterface::singleton().reload_scene_from_path(&local);
    }
}

/// Loaded .gdextension files name their libraries per platform; any of
/// those changing on disk means that extension was rebuilt.
fn extension_map() -> HashMap<PathBuf, GString> {
    let mut map = HashMap::new();
    for extension in GDExtensionManager::singleton().get_loaded_extensions().as_slice() {
        let mut config = ConfigFile::new_gd();
        if config.load(extension) != godot::global::Error::OK {
            continue;
        }
        if !config.has_section("libraries") {
            continue;
        }
        for key in config.get_section_keys("libraries").as_slice() {
            let library = config.get_value("libraries", key).to_string();
            map.insert(globalize(&library), extension.clone());
        }
    }
    map
}

fn globalized(paths: &PackedStringArray) -> HashSet<PathBuf> {
    paths
        .as_slice()
        .iter()
        .map(|path| globalize(&path.to_string()))
        .collect()
}

fn globalize(path: &str) -> PathBuf {
    PathBuf::from(
        ProjectSettings::singleton()
            .globalize_path(path)
            .to_string(),
    )
}

fn localize(path: &Path) -> Option<GString> {
    Some(ProjectSettings::singleton().localize_path(path.to_str()?))
}

fn project_root() -> Option<PathBuf> {
    let root = ProjectSettings::singleton()
        .globalize_path("res://")
        .to_string();
    (!root.is_empty()).then(|| PathBuf::from(root))
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
}
