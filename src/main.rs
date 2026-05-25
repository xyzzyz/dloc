fn main() {
    if let Err(err) = dloc::main_entry() {
        eprintln!("dloc: {err}");
        std::process::exit(1);
    }
}
