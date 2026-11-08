fn main() {
    let bytes = std::fs::read(std::env::args_os().nth(1).expect("input file"))
        .expect("read input");
    let _ = vesper_format::PackFrame::read_from(bytes.as_slice(), vesper_format::MAX_FRAME_RAW_SIZE);
}
