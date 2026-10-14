fn main() {
    if let Err(error) = rustfrp_desktop::run() {
        eprintln!("RustFRP desktop failed to start: {error}");
        std::process::exit(1);
    }
}
