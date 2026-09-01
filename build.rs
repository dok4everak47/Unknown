// Link the final binary with -no_exported_symbols (dead metadata removal).
//
// This binary is an executable: nothing dynamically links against it, so every
// export in the LC_DYLD_EXPORTS_TRIE (~575 BoringSSL API symbols after the
// -fvisibility=hidden CFLAGS pass, ~11.4 KB of pure metadata) is dead weight.
// -no_exported_symbols empties the trie entirely (probe: 11,472 -> 8 B) while
// leaving all internal symbol resolution and the 202 undefined framework
// imports untouched (those are imports, not exports, and are unaffected).
//
// Scoped to BIN targets only via cargo:rustc-link-arg-bins, so the proc-macro
// dylibs (displaydoc, proc-macro2) that NEED their exports are not affected —
// this is why the earlier global -no_exported_symbols attempt (exp #32) broke
// the build. Behavior is fully preserved (verified: binary runs, checks pass).
//
// Flag form depends on the linker:
// - Apple ld64 (the nix devShell sets CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER
//   to the CommandLineTools ld, see flake.nix) takes the BARE flag and rejects
//   the -Wl, prefix ("ld: unknown options: -Wl,-no_exported_symbols").
// - Plain (non-devShell) cargo builds link through the cc/clang driver, which
//   rejects the bare flag but forwards `-Wl,-no_exported_symbols` to the
//   underlying cctools/system ld64 (both accept -no_exported_symbols).
// We therefore emit the bare flag only when the target linker is a direct Apple
// ld64, and the driver-safe -Wl, form otherwise, so plain builds keep working.
fn main() {
    let linker = std::env::var("CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER").unwrap_or_default();
    if linker.ends_with("/usr/bin/ld") {
        println!("cargo:rustc-link-arg-bins=-no_exported_symbols");
    } else {
        println!("cargo:rustc-link-arg-bins=-Wl,-no_exported_symbols");
    }

    // Apple ld64 (the devShell release linker) does not search nix store paths,
    // so it needs libiconv's devShell dir via -L. The nix devShell injects the
    // path dynamically as MYAGENT_LIBICONV_LIB_DIR (see flake.nix) — no hardcoded
    // store hash. We emit cargo:rustc-link-arg (not -bins) so the -L also applies
    // when linking test binaries (cargo test links through the same ld64).
    // Note: this only affects myagent's own artifacts; dependency build scripts
    // (aws-lc-sys) link -liconv through the same ld64 and get the search path from
    // the devShell's CARGO_TARGET_AARCH64_APPLE_DARWIN_RUSTFLAGS (see flake.nix).
    // When unset (plain macOS builds where iconv lives in libSystem), we emit
    // nothing and the build is unaffected.
    if let Ok(dir) = std::env::var("MYAGENT_LIBICONV_LIB_DIR")
        && std::path::Path::new(&dir).is_dir()
    {
        println!("cargo:rustc-link-arg=-L{}", dir);
    }
    println!("cargo:rerun-if-env-changed=MYAGENT_LIBICONV_LIB_DIR");
}
