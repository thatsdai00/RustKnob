fn main() {
    // which build this is: an installed copy with an older stamp gets updated by a newer exe
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    println!("cargo:rustc-env=RK_BUILD={stamp}");
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=assets/app.rc");
    println!("cargo:rerun-if-changed=assets/rustknob.ico");
    let _ = embed_resource::compile("assets/app.rc", embed_resource::NONE);
}
