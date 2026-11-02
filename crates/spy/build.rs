fn main() {
    // Keep every #[no_mangle] hook export. The MSVC linker's Identical COMDAT
    // Folding (/OPT:ICF) merges byte-identical functions and drops the extra
    // export name, which silently removed `paxile_callwndret_proc` whenever its
    // body matched another hook. Disable ICF for this cdylib so all three hook
    // entry points stay exported regardless of their bodies.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-cdylib=/OPT:NOICF");
    }
}
