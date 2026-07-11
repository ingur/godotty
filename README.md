<p align="center">
  <h1 align="center">Godotty</h1>
</p>

<img width="2560" height="1440" alt="godotty" src="https://github.com/user-attachments/assets/b3d2b8f8-1e39-4a9c-bbf7-2752c6660e07" />

<p align="center">
  A full-featured yet light-weight terminal emulator for the <a href=https://godotengine.org>Godot Engine</a>, powered by <a href=https://ghostty.org>libghostty</a>.
</p>

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://shieldcn.dev/github/ingur/godotty/release.svg?font=jetbrains-mono&mode=dark">
    <img src="https://shieldcn.dev/github/ingur/godotty/release.svg?font=jetbrains-mono&mode=light" alt="Version">
  </picture>
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://shieldcn.dev/badge/runs%20on-linux-green.svg?logo=linux&font=jetbrains-mono&mode=dark">
    <img src="https://shieldcn.dev/badge/runs%20on-linux-green.svg?logo=linux&font=jetbrains-mono&mode=light" alt="Runs on Linux">
  </picture>
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://shieldcn.dev/badge/runs%20on-macos-black.svg?logo=apple&font=jetbrains-mono&mode=dark">
    <img src="https://shieldcn.dev/badge/runs%20on-macos-black.svg?logo=apple&font=jetbrains-mono&mode=light" alt="Runs on macOS">
  </picture>
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://shieldcn.dev/badge/runs%20on-windows-blue.svg?font=jetbrains-mono&mode=dark">
    <img src="https://shieldcn.dev/badge/runs%20on-windows-blue.svg?font=jetbrains-mono&mode=light" alt="Runs on Windows">
  </picture>
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://shieldcn.dev/github/ingur/godotty/license.svg?font=jetbrains-mono&mode=dark">
    <img src="https://shieldcn.dev/github/ingur/godotty/license.svg?font=jetbrains-mono&mode=light" alt="License">
  </picture>
</p>

## Features

- Full terminal emulation powered by [ghostty](https://ghostty.org)'s VT
  engine ([libghostty-vt](https://github.com/Uzaaft/libghostty-rs))
- Terminals anywhere in the editor: movable docks, a tabbed/toggleable bottom panel,
  and as a main screen
- Selection, clipboard copy / paste (Ctrl+Shift+C / V, Cmd+C / V on macOS),
  drag to reorder tabs
- Tab titles follow the running program, shells open as login shells in
  your project root
- Terminal colors are auto-generated from your editor theme
- JetBrains Mono Nerd Fonts and emojis built in, no setup
- A `Terminal` node for use inside your own editor tools
- Written in Rust as a self-contained GDExtension

## Install

Requires Godot 4.7+ on Linux, macOS, or Windows 10+.

- **Asset Library**: search for "Godotty" in the editor's AssetLib tab and
  install it.
- **Manual**: grab the zip from the
  [releases page](https://github.com/ingur/godotty/releases) and extract it
  into your project root.

> [!NOTE]
> On macOS, if Gatekeeper blocks the library,
> clear the quarantine flag: 
> `xattr -dr com.apple.quarantine addons/godotty`.

## Quick Start

- Ctrl+Shift+P and type "terminal" to create one in a dock, the bottom panel, or the main screen.
- Ctrl+` toggles the terminal panel in the bottom panel.
- Drag docks anywhere, or float them as windows.
- Settings live in Editor Settings under `godotty`.

## Exports

Godotty never ships in exported games.
You can silence the harmless startup logs about the missing extension by adding `addons/godotty/*` to your export preset's exclude filter.
To ship the `Terminal` node in your game instead, remove the `.editor` tags in `godotty.gdextension`.

## Why?

- I wanted to be able to use all my terminal tools from inside Godot.
- I wanted to build something that feels native to the editor.
- I wanted to use libghostty.

## Building from source

Requirements: Rust (stable), Zig 0.15.2, or
[devenv](https://devenv.sh) with the included configuration.

```bash
cargo build --release   # produces target/release/libgodotty.so
package                 # devenv script: assembles dist/godotty-v*.zip
```

## Roadmap

- Kitty image protocol support, still need to hook it up.

## Credits

- [Godot](https://godotengine.org) for the engine
- [ghostty](https://ghostty.org) for `libghostty-vt`
- [libghostty-rs](https://github.com/Uzaaft/libghostty-rs) for the Rust bindings
- [JetBrains Mono](https://www.jetbrains.com/lp/mono/) for the bundled fonts

## License

[MIT](LICENSE), Bundled fonts are OFL-1.1, see
[addons/godotty/FONTS.md](addons/godotty/FONTS.md).
