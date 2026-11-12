fn main() {
    if omk_skroot_loader::runtime::worker().is_err() {
        std::process::exit(1);
    }
}
