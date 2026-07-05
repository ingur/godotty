// libghostty-vt-sys installs ghostty-vt-static.lib, but rustc on windows-gnu
// links libghostty-vt.a; alias it. Delete once fixed upstream.
fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    if !target.contains("windows") {
        return;
    }
    let Ok(include) = std::env::var("DEP_GHOSTTY_VT_INCLUDE") else {
        return;
    };
    let lib = std::path::Path::new(&include).parent().unwrap().join("lib");
    let src = lib.join("ghostty-vt-static.lib");
    let dst = lib.join("libghostty-vt.a");
    if src.exists() && !dst.exists() {
        let _ = std::fs::copy(&src, &dst);
    }
    println!("cargo:rustc-link-search=native={}", lib.display());
}
