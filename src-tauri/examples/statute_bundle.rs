fn main() {
    if let Err(err) = argos_lib::statute::bundle_main() {
        eprintln!("statute bundle failed: {err}");
        std::process::exit(1);
    }
}
