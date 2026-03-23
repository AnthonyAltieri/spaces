fn main() {
    if let Err(error) = spaces::run() {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}
