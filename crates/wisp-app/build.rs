// Embers talks to its server over a Unix socket, so the backend is only
// compiled in on Unix targets. `cfg(embers)` combines that with the feature.
fn main() {
    println!("cargo::rustc-check-cfg=cfg(embers)");
    if std::env::var_os("CARGO_FEATURE_EMBERS").is_some()
        && std::env::var_os("CARGO_CFG_UNIX").is_some()
    {
        println!("cargo::rustc-cfg=embers");
    }
}
