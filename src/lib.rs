mod font;
mod input;
mod plugin;
mod pty;
mod sprite;
mod sync;
mod terminal;
mod theme;

use godot::prelude::*;

struct GodottyExtension;

#[gdextension]
unsafe impl ExtensionLibrary for GodottyExtension {}
