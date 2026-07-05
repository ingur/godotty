{ pkgs, lib, ... }:

{
  packages = with pkgs; [
    git
    pkg-config

    # libghostty-vt-sys builds libghostty-vt.a from ghostty's Zig sources at
    # `cargo build` time (it git-clones a pinned ghostty commit into OUT_DIR and
    # runs `zig build -Demit-lib-vt`). That pinned commit requires Zig 0.15.2,
    # nixpkgs `zig` is 0.16 which won't build it, so pin the 0.15 series.
    zig_0_15
  ];

  languages.rust = {
    enable = true;
    channel = "stable"; # rust-overlay latest stable; satisfies gdext MSRV 1.94
    # for `cargo check --target x86_64-pc-windows-gnu`; linking happens in CI
    targets = [ "x86_64-pc-windows-gnu" ];
  };

  # Debug cargo builds would otherwise zig-build libghostty in Debug mode.
  env.LIBGHOSTTY_VT_SYS_OPTIMIZE = "ReleaseFast";

  # Convenience wrappers; run these inside the devenv shell.
  scripts.build.exec = "cargo build";
  scripts.package.exec = ''
    set -euo pipefail
    version=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
    cargo build --release
    mkdir -p addons/godotty/bin dist
    cp target/release/libgodotty.so addons/godotty/bin/libgodotty.linux.x86_64.so
    rm -f "dist/godotty-v$version.zip"
    zip -qr "dist/godotty-v$version.zip" addons
    echo "dist/godotty-v$version.zip"
  '';
}
