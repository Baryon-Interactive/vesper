fn main() {
    let input = std::fs::read_to_string(std::env::args_os().nth(1).expect("input file"))
        .expect("read input");
    let _ = vesper_format::validate_relative_path(&input);
}
