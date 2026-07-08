// libghostty-vt-sys installs ghostty-vt-static.lib, but rustc on windows-gnu
// links libghostty-vt.a; alias it. Delete once fixed upstream.
fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    if !target.contains("windows") {
        return;
    }

    // Link MSVC runtime libraries. Only applies to MSVC targets; MinGW provides its own.
    // vcruntime: memcpy, memset, memmove, __CxxFrameHandler3, _CxxThrowException
    // ucrt: ceilf, floorf, sinf, cosf, strlen, free, ...
    if target.contains("msvc") {
        println!("cargo:rustc-link-lib=dylib=vcruntime");
        println!("cargo:rustc-link-lib=dylib=ucrt");
    }

    let Ok(include) = std::env::var("DEP_GHOSTTY_VT_INCLUDE") else {
        return;
    };
    let Some(parent) = std::path::Path::new(&include).parent() else {
        return;
    };
    let lib = parent.join("lib");
    let src = lib.join("ghostty-vt-static.lib");
    let dst = lib.join("libghostty-vt.a");
    if src.exists() && !dst.exists() {
        let _ = std::fs::copy(&src, &dst);
    }
    println!("cargo:rustc-link-search=native={}", lib.display());
}
