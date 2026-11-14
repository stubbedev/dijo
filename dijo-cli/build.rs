fn main() {
    let target = |key| std::env::var(key).ok();
    if target("CARGO_CFG_TARGET_OS").as_deref() == Some("linux")
        && target("CARGO_CFG_TARGET_ENV").as_deref() == Some("gnu")
    {
        println!("cargo:rustc-link-arg-bins=-Wl,-z,pack-relative-relocs");
    }
}
