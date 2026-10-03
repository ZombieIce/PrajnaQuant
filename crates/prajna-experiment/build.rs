fn main() {
    let target = std::env::var("TARGET").expect("cargo sets TARGET for build scripts");
    println!("cargo:rustc-env=PRAJNA_BUILD_TARGET={target}");
}
