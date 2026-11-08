fn main() {
    let bytes = std::fs::read(std::env::args_os().nth(1).expect("input file"))
        .expect("read input");
    let _ = vesper_format::Manifest::decode(&bytes);
}
