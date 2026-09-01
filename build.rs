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
fn main() {
    println!("cargo:rustc-link-arg-bins=-no_exported_symbols");
}
