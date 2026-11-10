fn main() {
    if let Err(reason) = r1_runtime::run(std::env::args().skip(1)) {
        eprintln!("r1-helper: {}", r1_runtime::single_line(&reason));
        std::process::exit(1);
    }
}
