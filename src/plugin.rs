use godot::classes::control::SizeFlags;
use godot::classes::editor_plugin::DockSlot;
use godot::classes::notify::ContainerNotification;
use godot::classes::object::ConnectFlags;
use godot::classes::tab_bar::CloseButtonDisplayPolicy;
use godot::classes::{
    Button, CenterContainer, Control, EditorExportPlugin, EditorInterface, EditorPlugin,
    EditorSettings, HBoxContainer, IEditorExportPlugin, IEditorPlugin, IVBoxContainer, InputEvent,
    InputEventKey, Label, MarginContainer, PanelContainer, Shortcut, TabBar, TabContainer,
    Texture2D, VBoxContainer,
};
use godot::global::{HorizontalAlignment, Key as GKey};
use godot::prelude::*;
use godot::register::info::PropertyHint;

use crate::sync::SyncNode;
use crate::terminal::Terminal;

/// Editor chords the terminal leaves unconsumed, comma separated.
pub(crate) const PASSTHROUGH_SETTING: &str = "godotty/terminal/passthrough_shortcuts";
#[cfg(target_os = "macos")]
const PASSTHROUGH_DEFAULT: &str = "Meta+Shift+P";
#[cfg(not(target_os = "macos"))]
const PASSTHROUGH_DEFAULT: &str = "Ctrl+Shift+P";

/// Editor shortcut (Editor Settings > Shortcuts) toggling the bottom
/// terminal panel: show and focus its active terminal, or hide it and
/// return focus to where you were.
pub(crate) const TOGGLE_SHORTCUT: &str = "godotty/toggle_terminal_panel";

/// Per-terminal zoom, transient like ghostty's.
pub(crate) const INCREASE_FONT_SHORTCUT: &str = "godotty/increase_font_size";
pub(crate) const DECREASE_FONT_SHORTCUT: &str = "godotty/decrease_font_size";
pub(crate) const RESET_FONT_SHORTCUT: &str = "godotty/reset_font_size";

const SHELL_SETTING: &str = "godotty/terminal/shell";
const FONT_SIZE_SETTING: &str = "godotty/terminal/font_size";
pub(crate) const COLOR_SCHEME_SETTING: &str = "godotty/terminal/color_scheme";
pub(crate) const LIGATURES_SETTING: &str = "godotty/terminal/ligatures";

const SYNC_SETTING: &str = "godotty/terminal/sync_external_changes";
const DOCK_SLOT_SETTING: &str = "godotty/terminal/default_dock_slot";
const DOCK_SLOT_DEFAULT: i32 = 0; // DockSlot::LEFT_UL
const DOCK_SLOT_NAMES: &str = "Far Left Top,Far Left Bottom,Left Top (Scene),\
Left Bottom (FileSystem),Right Top (Inspector),Right Bottom,Far Right Top,Far Right Bottom";

/// Tabbed terminals for the bottom panel and the main screen: one row with
/// the tab strip and a new-terminal button, terminals stacked below.
#[derive(GodotClass)]
#[class(tool, init, base = VBoxContainer, internal)]
pub struct TerminalTabs {
    base: Base<VBoxContainer>,
    bar: Option<Gd<TabBar>>,
    stack: Option<Gd<MarginContainer>>,
    empty: Option<Gd<CenterContainer>>,
}

#[godot_api]
impl TerminalTabs {
    #[func]
    pub fn add_terminal(&mut self) {
        if self.stack.is_none() || self.bar.is_none() {
            return;
        }
        let terminal = new_wired_terminal(self.to_gd().upcast());
        let (Some(stack), Some(bar)) = (self.stack.as_mut(), self.bar.as_mut()) else {
            return;
        };
        stack.add_child(&terminal);
        bar.add_tab_ex().title(&default_title()).done();
        let last = bar.get_tab_count() - 1;
        // Tabs carry the terminal's identity so order can always be resynced.
        bar.set_tab_metadata(last, &terminal.instance_id().to_i64().to_variant());
        bar.set_current_tab(last);
        self.show_only(last);
        self.update_empty();
    }

    /// Focus the active tab's terminal, creating one when there is none.
    pub fn focus_active(&mut self) {
        let Some(bar) = self.bar.as_ref() else { return };
        let tab = bar.get_current_tab();
        if tab < 0 {
            self.add_terminal();
            return;
        }
        let Some(stack) = self.stack.as_ref() else {
            return;
        };
        if let Some(mut child) = stack.get_child(tab) {
            child.call_deferred("grab_focus", &[]);
        }
    }

    #[func]
    fn deferred_autocreate(&mut self) {
        let empty = self
            .bar
            .as_ref()
            .is_some_and(|bar| bar.get_tab_count() == 0);
        if empty && self.base().is_visible_in_tree() {
            self.add_terminal();
        }
    }

    #[func]
    fn on_tab_changed(&mut self, tab: i32) {
        self.show_only(tab);
    }

    #[func]
    fn on_tab_close(&mut self, tab: i32) {
        self.close_tab(tab);
    }

    /// Variant argument: deferred delivery may outlive the terminal.
    #[func]
    fn on_title_changed(&mut self, title: GString, terminal: Variant) {
        let Ok(terminal) = terminal.try_to::<Gd<Terminal>>() else {
            return;
        };
        if title.is_empty() {
            return;
        }
        let tab = terminal.get_index();
        let Some(bar) = self.bar.as_mut() else { return };
        if tab >= 0 && tab < bar.get_tab_count() {
            bar.set_tab_title(tab, &title);
        }
    }

    #[func]
    fn on_exited(&mut self, _code: i64, terminal: Variant) {
        let Ok(terminal) = terminal.try_to::<Gd<Terminal>>() else {
            return;
        };
        self.close_tab(terminal.get_index());
    }

    /// TabBar reorders any dragged tab, active or not; rebuild the stack
    /// order from the identities the tabs carry.
    #[func]
    fn on_tab_rearranged(&mut self, _to: i32) {
        let (Some(stack), Some(bar)) = (self.stack.as_mut(), self.bar.as_mut()) else {
            return;
        };
        for tab in 0..bar.get_tab_count() {
            let Ok(id) = bar.get_tab_metadata(tab).try_to::<i64>() else {
                continue;
            };
            for i in tab..stack.get_child_count() {
                let Some(child) = stack.get_child(i) else {
                    continue;
                };
                if child.instance_id().to_i64() == id {
                    if i != tab {
                        stack.move_child(&child, tab);
                    }
                    break;
                }
            }
        }
        let current = self.bar.as_ref().map_or(-1, |bar| bar.get_current_tab());
        self.show_only(current);
    }

    fn close_tab(&mut self, tab: i32) {
        let (Some(stack), Some(bar)) = (self.stack.as_mut(), self.bar.as_mut()) else {
            return;
        };
        if tab < 0 || tab >= bar.get_tab_count() {
            return;
        }
        if let Some(child) = stack.get_child(tab) {
            stack.remove_child(&child);
            child.free();
        }
        bar.remove_tab(tab);
        self.update_empty();
    }

    fn update_empty(&mut self) {
        let count = self.bar.as_ref().map_or(0, |bar| bar.get_tab_count());
        if let Some(empty) = self.empty.as_mut() {
            empty.set_visible(count == 0);
        }
    }

    fn show_only(&mut self, tab: i32) {
        let Some(stack) = self.stack.as_mut() else {
            return;
        };
        for i in 0..stack.get_child_count() {
            let Some(child) = stack.get_child(i) else {
                continue;
            };
            let Ok(mut control) = child.try_cast::<Control>() else {
                continue;
            };
            control.set_visible(i == tab);
            if i == tab {
                control.call_deferred("grab_focus", &[]);
            }
        }
    }
}

#[godot_api]
impl IVBoxContainer for TerminalTabs {
    fn ready(&mut self) {
        if self.bar.is_some() {
            return;
        }
        let mut row = HBoxContainer::new_alloc();
        let mut bar = TabBar::new_alloc();
        bar.set_clip_tabs(false);
        bar.set_max_tab_width(200);
        bar.set_tab_close_display_policy(CloseButtonDisplayPolicy::SHOW_ALWAYS);
        bar.set_drag_to_rearrange_enabled(true);
        bar.set_theme_type_variation("TabContainer");
        bar.connect_flags(
            "tab_changed",
            &self.to_gd().callable("on_tab_changed"),
            ConnectFlags::DEFERRED,
        );
        bar.connect("tab_close_pressed", &self.to_gd().callable("on_tab_close"));
        bar.connect_flags(
            "active_tab_rearranged",
            &self.to_gd().callable("on_tab_rearranged"),
            ConnectFlags::DEFERRED,
        );
        row.add_child(&bar);
        let mut add = Button::new_alloc();
        add.set_tooltip_text("New terminal");
        add.set_flat(true);
        match editor_icon("Add") {
            Some(icon) => add.set_button_icon(&icon),
            None => add.set_text("+"),
        }
        add.connect("pressed", &self.to_gd().callable("add_terminal"));
        row.add_child(&add);
        self.base_mut().add_child(&row);

        let mut panel = PanelContainer::new_alloc();
        panel.set_v_size_flags(SizeFlags::EXPAND_FILL);
        let stack = MarginContainer::new_alloc();
        panel.add_child(&stack);
        let mut empty = CenterContainer::new_alloc();
        let mut empty_box = VBoxContainer::new_alloc();
        let mut label = Label::new_alloc();
        label.set_text("No open terminals");
        label.set_horizontal_alignment(HorizontalAlignment::CENTER);
        label.set_modulate(Color::from_rgba(1.0, 1.0, 1.0, 0.6));
        empty_box.add_child(&label);
        let mut new_button = Button::new_alloc();
        new_button.set_text("New Terminal");
        if let Some(icon) = editor_icon("Add") {
            new_button.set_button_icon(&icon);
        }
        new_button.connect("pressed", &self.to_gd().callable("add_terminal"));
        empty_box.add_child(&new_button);
        empty.add_child(&empty_box);
        panel.add_child(&empty);
        self.base_mut().add_child(&panel);
        self.bar = Some(bar);
        self.stack = Some(stack);
        self.empty = Some(empty);
    }

    /// A visited empty tab area creates its first terminal.
    fn on_notification(&mut self, what: ContainerNotification) {
        if what != ContainerNotification::VISIBILITY_CHANGED {
            return;
        }
        let empty = self
            .bar
            .as_ref()
            .is_some_and(|bar| bar.get_tab_count() == 0);
        if empty && self.base().is_visible_in_tree() {
            // Deferred: this fires while the editor applies layout, and
            // spawning mid-layout crashes the first boot of a project.
            self.base_mut().call_deferred("deferred_autocreate", &[]);
        }
    }
}

/// Terminals in three editor locations: per-terminal docks (native drag,
/// float, tabbing), a tabbed bottom panel, and a tabbed main screen.
#[derive(GodotClass)]
#[class(tool, init, base = EditorPlugin, internal)]
struct TerminalPanel {
    base: Base<EditorPlugin>,
    terminals: Vec<Gd<Terminal>>,
    count: u32,
    panel: Option<Gd<TerminalTabs>>,
    main: Option<Gd<TerminalTabs>>,
    last_other: Option<Gd<Control>>,
    sync: Option<Gd<SyncNode>>,
    export_plugin: Option<Gd<GodottyExportPlugin>>,
}

#[godot_api]
impl TerminalPanel {
    #[func]
    fn cmd_new_dock_terminal(&mut self) {
        if self.terminals.is_empty() {
            self.count = 0;
        }
        self.count += 1;
        let title = if self.count == 1 {
            default_title()
        } else {
            format!("{} ({})", default_title(), self.count)
        };

        let mut terminal = new_wired_terminal(self.to_gd().upcast());
        terminal.set_name(title.as_str());
        terminal.set_custom_minimum_size(Vector2::new(200.0, 150.0));

        let slot = EditorInterface::singleton()
            .get_editor_settings()
            .map(|s| s.get_setting(DOCK_SLOT_SETTING))
            .and_then(|v| v.try_to::<i64>().ok())
            .and_then(|v| DockSlot::try_from_ord(v as i32))
            .unwrap_or(DockSlot::LEFT_UL);
        self.base_mut().add_control_to_dock(slot, &terminal);
        terminal.call_deferred("grab_focus", &[]);
        self.terminals.push(terminal);
    }

    #[func]
    fn cmd_new_panel_terminal(&mut self) {
        let Some(panel) = self.panel.clone() else {
            return;
        };
        // Create first: revealing an empty panel auto-creates one already.
        panel.clone().bind_mut().add_terminal();
        self.base_mut().make_bottom_panel_item_visible(&panel);
    }

    #[func]
    fn cmd_new_main_terminal(&mut self) {
        let Some(main) = self.main.clone() else {
            return;
        };
        // Deferred: switching while the command palette closes gets undone.
        EditorInterface::singleton()
            .call_deferred("set_main_screen_editor", &["Terminal".to_variant()]);
        main.clone().bind_mut().add_terminal();
    }

    /// Variant argument: deferred delivery may outlive the terminal.
    #[func]
    fn on_exited(&mut self, _code: i64, terminal: Variant) {
        let Ok(terminal) = terminal.try_to::<Gd<Terminal>>() else {
            return;
        };
        self.base_mut().remove_control_from_docks(&terminal);
        self.terminals.retain(|t| *t != terminal);
        terminal.free();
    }

    /// The editor saving a scene must not read back as an external change.
    #[func]
    fn on_scene_saved(&mut self, path: GString) {
        if let Some(node) = self.sync.as_mut() {
            node.bind_mut().note_scene_saved(&path.to_string());
        }
    }

    #[func]
    fn on_editor_settings_changed(&mut self) {
        self.update_sync();
    }

    /// Start or stop external change syncing to match the editor setting.
    /// The watcher lives in its own node so its reloads never run while this
    /// plugin is bound.
    fn update_sync(&mut self) {
        let enabled = EditorInterface::singleton()
            .get_editor_settings()
            .map(|s| s.get_setting(SYNC_SETTING))
            .and_then(|v| v.try_to::<bool>().ok())
            .unwrap_or(true);
        if enabled && self.sync.is_none() {
            let node = SyncNode::new_alloc();
            self.base_mut().add_child(&node);
            self.sync = Some(node);
        } else if !enabled && let Some(mut node) = self.sync.take() {
            node.queue_free();
        }
    }

    /// Remember where focus was outside the tab containers, so the toggle
    /// can return there. Dock terminals count: they are a place you work.
    #[func]
    fn on_focus_changed(&mut self, control: Variant) {
        let Ok(control) = control.try_to::<Gd<Control>>() else {
            return;
        };
        let node: Gd<Node> = control.clone().upcast();
        let ours = [self.panel.as_ref(), self.main.as_ref()]
            .into_iter()
            .flatten()
            .any(|tabs| is_descendant(&node, &tabs.clone().upcast()));
        if !ours {
            self.last_other = Some(control);
        }
    }

    /// Toggle the bottom terminal panel: from inside it, hide it and return
    /// focus; from anywhere else, show it and focus its active terminal.
    fn toggle_terminal_focus(&mut self) {
        let Some(panel) = self.panel.clone() else {
            return;
        };
        let in_panel = self
            .base()
            .get_viewport()
            .and_then(|v| v.gui_get_focus_owner())
            .is_some_and(|f| is_descendant(&f.upcast(), &panel.clone().upcast()));

        if in_panel {
            self.base_mut().hide_bottom_panel();
            if let Some(mut other) = self.last_other.clone().filter(Gd::is_instance_valid) {
                other.call_deferred("grab_focus", &[]);
            }
        } else {
            // Showing an empty panel auto-creates and focuses a terminal.
            self.base_mut().make_bottom_panel_item_visible(&panel);
            panel.clone().bind_mut().focus_active();
        }
    }

    /// Nearest ancestor dock tab when docked, window title when floating.
    /// Variant argument: deferred delivery may outlive the terminal.
    #[func]
    fn on_title_changed(&mut self, title: GString, terminal: Variant) {
        let Ok(terminal) = terminal.try_to::<Gd<Terminal>>() else {
            return;
        };
        if title.is_empty() {
            return;
        }
        let mut child: Gd<Node> = terminal.clone().upcast();
        while let Some(parent) = child.get_parent() {
            if let Ok(mut tabs) = parent.clone().try_cast::<TabContainer>() {
                if let Ok(control) = child.try_cast::<Control>() {
                    let tab = tabs.get_tab_idx_from_control(&control);
                    if tab >= 0 {
                        tabs.set_tab_title(tab, &title);
                    }
                }
                return;
            }
            child = parent;
        }
        let editor_window = EditorInterface::singleton()
            .get_base_control()
            .and_then(|base| base.get_window());
        let floating = terminal
            .get_window()
            .filter(|window| Some(window) != editor_window.as_ref());
        if let Some(mut window) = floating {
            window.set_title(&title);
        }
    }
}

#[godot_api]
impl IEditorPlugin for TerminalPanel {
    fn enter_tree(&mut self) {
        if let Some(mut palette) = EditorInterface::singleton().get_command_palette() {
            let this = self.to_gd();
            palette.add_command(
                "Terminal: New Terminal in Dock",
                "godotty/new_dock",
                &this.callable("cmd_new_dock_terminal"),
            );
            palette.add_command(
                "Terminal: New Terminal in Bottom Panel",
                "godotty/new_panel",
                &this.callable("cmd_new_panel_terminal"),
            );
            palette.add_command(
                "Terminal: New Terminal in Main Screen",
                "godotty/new_main",
                &this.callable("cmd_new_main_terminal"),
            );
        }
        if let Some(mut settings) = EditorInterface::singleton().get_editor_settings() {
            // Registration order is display order.
            ensure_setting(&mut settings, SHELL_SETTING, &GString::new().to_variant());
            add_hint(
                &mut settings,
                SHELL_SETTING,
                VariantType::STRING,
                PropertyHint::PLACEHOLDER_TEXT,
                "program, no arguments; empty = auto",
            );
            ensure_setting(&mut settings, FONT_SIZE_SETTING, &14i64.to_variant());
            add_hint(
                &mut settings,
                FONT_SIZE_SETTING,
                VariantType::INT,
                PropertyHint::RANGE,
                "6,32,1",
            );
            ensure_setting(&mut settings, COLOR_SCHEME_SETTING, &0i64.to_variant());
            add_hint(
                &mut settings,
                COLOR_SCHEME_SETTING,
                VariantType::INT,
                PropertyHint::ENUM,
                "Auto,Dark,Light",
            );
            ensure_setting(&mut settings, LIGATURES_SETTING, &true.to_variant());
            ensure_setting(
                &mut settings,
                DOCK_SLOT_SETTING,
                &DOCK_SLOT_DEFAULT.to_variant(),
            );
            add_hint(
                &mut settings,
                DOCK_SLOT_SETTING,
                VariantType::INT,
                PropertyHint::ENUM,
                DOCK_SLOT_NAMES,
            );
            ensure_setting(
                &mut settings,
                PASSTHROUGH_SETTING,
                &GString::from(PASSTHROUGH_DEFAULT).to_variant(),
            );
            ensure_setting(&mut settings, SYNC_SETTING, &true.to_variant());
            ensure_shortcut(&mut settings, TOGGLE_SHORTCUT, GKey::QUOTELEFT, true, false);
            // Standard zoom chords, Cmd on macOS.
            let meta = cfg!(target_os = "macos");
            ensure_shortcut(&mut settings, INCREASE_FONT_SHORTCUT, GKey::EQUAL, !meta, meta);
            ensure_shortcut(&mut settings, DECREASE_FONT_SHORTCUT, GKey::MINUS, !meta, meta);
            ensure_shortcut(&mut settings, RESET_FONT_SHORTCUT, GKey::KEY_0, !meta, meta);
        }

        let mut panel = TerminalTabs::new_alloc();
        panel.set_custom_minimum_size(Vector2::new(0.0, 300.0));
        self.base_mut()
            .add_control_to_bottom_panel(&panel, "Terminal");
        self.panel = Some(panel);

        let mut main = TerminalTabs::new_alloc();
        main.set_v_size_flags(SizeFlags::EXPAND_FILL);
        main.set_h_size_flags(SizeFlags::EXPAND_FILL);
        main.set_visible(false);
        if let Some(mut screen) = EditorInterface::singleton().get_editor_main_screen() {
            screen.add_child(&main);
        }
        self.main = Some(main);

        self.base_mut().set_process_shortcut_input(true);
        if let Some(mut viewport) = self.base().get_viewport() {
            // Deferred: focus changes we cause arrive while self is bound.
            viewport.connect_flags(
                "gui_focus_changed",
                &self.to_gd().callable("on_focus_changed"),
                ConnectFlags::DEFERRED,
            );
        }

        let on_saved = self.to_gd().callable("on_scene_saved");
        self.base_mut().connect("scene_saved", &on_saved);
        if let Some(mut settings) = EditorInterface::singleton().get_editor_settings() {
            settings.connect(
                "settings_changed",
                &self.to_gd().callable("on_editor_settings_changed"),
            );
        }
        self.update_sync();

        let export_plugin = GodottyExportPlugin::new_gd();
        self.base_mut().add_export_plugin(&export_plugin);
        self.export_plugin = Some(export_plugin);
    }

    fn exit_tree(&mut self) {
        if let Some(plugin) = self.export_plugin.take() {
            self.base_mut().remove_export_plugin(&plugin);
        }
        if let Some(mut node) = self.sync.take() {
            node.queue_free();
        }
        if let Some(mut palette) = EditorInterface::singleton().get_command_palette() {
            palette.remove_command("godotty/new_dock");
            palette.remove_command("godotty/new_panel");
            palette.remove_command("godotty/new_main");
        }
        if let Some(panel) = self.panel.take() {
            self.base_mut().remove_control_from_bottom_panel(&panel);
            panel.free();
        }
        if let Some(main) = self.main.take() {
            if let Some(mut parent) = main.get_parent() {
                parent.remove_child(&main);
            }
            main.free();
        }
        let terminals = std::mem::take(&mut self.terminals);
        for terminal in terminals {
            self.base_mut().remove_control_from_docks(&terminal);
            terminal.free();
        }
    }

    fn shortcut_input(&mut self, event: Gd<InputEvent>) {
        if !event.is_pressed() || event.is_echo() {
            return;
        }
        if !shortcut_matches(TOGGLE_SHORTCUT, &event) {
            return;
        }
        self.toggle_terminal_focus();
        if let Some(mut viewport) = self.base().get_viewport() {
            viewport.set_input_as_handled();
        }
    }

    fn has_main_screen(&self) -> bool {
        true
    }

    fn get_plugin_name(&self) -> GString {
        "Terminal".into()
    }

    fn get_plugin_icon(&self) -> Option<Gd<Texture2D>> {
        editor_icon("Terminal").or_else(|| editor_icon("Window"))
    }

    fn make_visible(&mut self, visible: bool) {
        if let Some(main) = self.main.as_mut() {
            main.set_visible(visible);
        }
    }
}

/// The library ships editor-only; also skip the addon files at export, or
/// the packed .gdextension errors at every exported-game launch.
#[derive(GodotClass)]
#[class(tool, init, base = EditorExportPlugin, internal)]
struct GodottyExportPlugin {
    base: Base<EditorExportPlugin>,
}

#[godot_api]
impl IEditorExportPlugin for GodottyExportPlugin {
    fn export_file(&mut self, path: GString, _type: GString, _features: PackedStringArray) {
        if path.to_string().starts_with("res://addons/godotty/") && editor_only_libraries() {
            self.base_mut().skip();
        }
    }

    // "!" sorts before the engine's "GDExtension" plugin, so skip() wins
    // before its missing-library warning.
    fn get_name(&self) -> GString {
        "!godotty".into()
    }

    // Defaultless trait methods; never called while begin_customize_* is false.
    fn customize_resource(&mut self, _res: Gd<Resource>, _path: GString) -> Option<Gd<Resource>> {
        None
    }

    fn customize_scene(&mut self, _scene: Gd<Node>, _path: GString) -> Option<Gd<Node>> {
        None
    }

    fn get_customization_configuration_hash(&self) -> u64 {
        0
    }
}

/// True while every [libraries] entry is editor-tagged; removing the editor
/// tag opts the addon back into normal game exports.
fn editor_only_libraries() -> bool {
    let mut config = godot::classes::ConfigFile::new_gd();
    if config.load("res://addons/godotty/godotty.gdextension") != godot::global::Error::OK
        || !config.has_section("libraries")
    {
        return true;
    }
    config
        .get_section_keys("libraries")
        .as_slice()
        .iter()
        .all(|key| key.to_string().split('.').any(|tag| tag == "editor"))
}

/// A Terminal with its title_changed/exited signals wired to the owner's
/// on_title_changed/on_exited, deferred because they are emitted while the
/// Terminal is mutably bound.
fn new_wired_terminal(owner: Gd<Object>) -> Gd<Terminal> {
    let mut terminal = Terminal::new_alloc();
    {
        let mut t = terminal.bind_mut();
        t.run_in_editor = true;
        if let Some(shell) = configured_shell() {
            t.shell = shell.as_str().into();
        }
        if let Some(size) = EditorInterface::singleton()
            .get_editor_settings()
            .map(|s| s.get_setting(FONT_SIZE_SETTING))
            .and_then(|v| v.try_to::<i64>().ok())
        {
            t.font_size = (size as i32).clamp(6, 72);
        }
    }
    for (signal, method) in [
        ("title_changed", "on_title_changed"),
        ("exited", "on_exited"),
    ] {
        let cb = owner
            .callable(method)
            .bindv(&varray![&terminal.to_variant()]);
        terminal.connect_flags(signal, &cb, ConnectFlags::DEFERRED);
    }
    terminal
}

/// Register with the default; erase-then-set makes registration order the
/// display order. Existing values of the right type survive, which also
/// migrates values from older releases.
fn ensure_setting(settings: &mut Gd<EditorSettings>, name: &str, default: &Variant) {
    let current = settings
        .has_setting(name)
        .then(|| settings.get_setting(name))
        .filter(|v| v.get_type() == default.get_type());
    settings.erase(name);
    settings.set_setting(name, current.as_ref().unwrap_or(default));
    settings.set_initial_value(name, default, false);
}

/// Always register the default; add_shortcut keeps saved user bindings and
/// records the default as "original", which the shortcuts dialog requires.
fn ensure_shortcut(
    settings: &mut Gd<EditorSettings>,
    path: &str,
    keycode: GKey,
    ctrl: bool,
    meta: bool,
) {
    let mut key = InputEventKey::new_gd();
    key.set_keycode(keycode);
    key.set_ctrl_pressed(ctrl);
    key.set_meta_pressed(meta);
    let mut shortcut = Shortcut::new_gd();
    shortcut.set_events(&varray![&key.to_variant()]);
    settings.add_shortcut(path, &shortcut);
}

fn add_hint(
    settings: &mut Gd<EditorSettings>,
    name: &str,
    ty: VariantType,
    hint: PropertyHint,
    hint_string: &str,
) {
    let mut info = VarDictionary::new();
    info.set("name", name);
    info.set("type", ty.ord());
    info.set("hint", hint.ord());
    info.set("hint_string", hint_string);
    settings.add_property_info(&info);
}

/// Match against the shortcut's events directly; EditorSettings
/// `is_shortcut` warns when the path is not in its runtime registry.
pub(crate) fn shortcut_matches(path: &str, event: &Gd<InputEvent>) -> bool {
    EditorInterface::singleton()
        .get_editor_settings()
        .and_then(|s| s.get_shortcut(path))
        .is_some_and(|sc| sc.matches_event(event))
}

fn is_descendant(node: &Gd<Node>, ancestor: &Gd<Node>) -> bool {
    let mut current = node.clone();
    while let Some(parent) = current.get_parent() {
        if &parent == ancestor {
            return true;
        }
        current = parent;
    }
    false
}

fn configured_shell() -> Option<String> {
    EditorInterface::singleton()
        .get_editor_settings()
        .map(|s| s.get_setting(SHELL_SETTING))
        .and_then(|v| v.try_to::<GString>().ok())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

fn default_title() -> String {
    let shell = configured_shell()
        .or_else(|| std::env::var("SHELL").ok())
        .unwrap_or_default();
    shell
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("shell")
        .to_string()
}

fn editor_icon(name: &str) -> Option<Gd<Texture2D>> {
    EditorInterface::singleton()
        .get_base_control()?
        .get_theme_icon_ex(name)
        .theme_type("EditorIcons")
        .done()
}
