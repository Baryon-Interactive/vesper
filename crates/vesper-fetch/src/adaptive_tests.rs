use super::*;
use std::{io::Write, net::TcpListener};
use vesper_format::{build_from_directory, ChunkingParams};

struct Server {
    base: String,
    delay: Arc<AtomicU64>,
    bytes: Arc<AtomicU64>,
    requests: Arc<AtomicUsize>,
    fail: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Server {
    fn new(pack: Vec<u8>, delay_ms: u64, first_delay_ms: u64) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let delay = Arc::new(AtomicU64::new(delay_ms));
        let bytes = Arc::new(AtomicU64::new(0));
        let requests = Arc::new(AtomicUsize::new(0));
        let fail = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (lag, traffic, count, broken, halt) = (
            delay.clone(),
            bytes.clone(),
            requests.clone(),
            fail.clone(),
            stop.clone(),
        );
        let pack = Arc::new(pack);
        let worker = thread::spawn(move || {
            let mut children = Vec::new();
            while !halt.load(Ordering::Acquire) {
                let Ok((mut socket, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(2));
                    continue;
                };
                let (pack, lag, traffic, count, broken) = (
                    pack.clone(),
                    lag.clone(),
                    traffic.clone(),
                    count.clone(),
                    broken.clone(),
                );
                children.push(thread::spawn(move || {
                    socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                    let mut data = Vec::new(); let mut buf = [0u8;4096];
                    while !data.windows(4).any(|w| w == b"\r\n\r\n") {
                        let n = socket.read(&mut buf).unwrap_or(0); if n == 0 { return; } data.extend_from_slice(&buf[..n]);
                    }
                    let number = count.fetch_add(1, Ordering::AcqRel);
                    if broken.load(Ordering::Acquire) { let _ = socket.write_all(b"HTTP/1.1 404 Missing\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"); return; }
                    let range = String::from_utf8_lossy(&data).lines().find_map(|l| { let (k,v) = l.split_once(':')?; k.eq_ignore_ascii_case("range").then(|| v.trim().trim_start_matches("bytes=").to_string()) }).unwrap();
                    let (start,end) = range.split_once('-').unwrap();
                    let start = start.parse::<usize>().unwrap(); let end = end.parse::<usize>().unwrap().min(pack.len()-1);
                    thread::sleep(Duration::from_millis(if number == 0 && first_delay_ms > 0 { first_delay_ms } else { lag.load(Ordering::Acquire) }));
                    let header = format!("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{end}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",pack.len(),end-start+1);
                    if socket.write_all(header.as_bytes()).is_ok() && socket.write_all(&pack[start..=end]).is_ok() { traffic.fetch_add((end-start+1) as u64, Ordering::AcqRel); }
                    let _ = socket.flush();
                    let _ = socket.shutdown(std::net::Shutdown::Write);
                }));
            }
            for child in children {
                child.join().unwrap();
            }
        });
        Self {
            base,
            delay,
            bytes,
            requests,
            fail,
            stop,
            worker: Some(worker),
        }
    }
    fn spec(&self, id: &str) -> SourceSpec {
        let mut spec = SourceSpec::new(Source::Http(self.base.clone()));
        spec.id = id.into();
        spec
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.worker.take().unwrap().join().unwrap();
    }
}

fn fixture() -> (tempfile::TempDir, vesper_format::BuildOutput, Vec<u8>) {
    let input = tempfile::tempdir().unwrap();
    let mut x = 17u64;
    let data: Vec<u8> = (0..4 * 1024 * 1024)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect();
    fs::write(input.path().join("game.bin"), data).unwrap();
    let build = tempfile::tempdir().unwrap();
    let output = build_from_directory(
        input.path(),
        build.path(),
        "test".into(),
        "windows-x64".into(),
        "1".into(),
        ChunkingParams {
            min_size: 32768,
            avg_size: 65536,
            max_size: 131072,
        },
        512 * 1024 * 1024,
    )
    .unwrap();
    let pack = fs::read(
        build
            .path()
            .join("packs")
            .join(&output.manifest.packs[0].pack_id),
    )
    .unwrap();
    (build, output, pack)
}

#[test]
fn second_faster_source_provides_at_least_eighty_percent() {
    let (_build, output, pack) = fixture();
    let slow = Server::new(pack.clone(), 100, 0);
    let fast = Server::new(pack, 2, 0);
    let fetcher = MultiSourceFetcher::new(vec![slow.spec("slow"), fast.spec("fast")]).unwrap();
    let dest = tempfile::tempdir().unwrap();
    fetcher
        .materialize_files(&output.manifest, &[0], dest.path())
        .unwrap();
    assert_eq!(fetcher.source_ranking()[0], "fast");
    let fast_bytes = fast.bytes.load(Ordering::Acquire);
    let total = fast_bytes + slow.bytes.load(Ordering::Acquire);
    assert!(
        fast_bytes * 100 >= total * 80,
        "fast={fast_bytes} total={total}"
    );
    assert!(fetcher.probe_bytes() > 0);
}

#[test]
fn slowing_primary_switches_without_oscillation() {
    let (_build, output, pack) = fixture();
    let primary = Server::new(pack.clone(), 2, 0);
    let backup = Server::new(pack, 20, 0);
    let fetcher =
        MultiSourceFetcher::new(vec![primary.spec("primary"), backup.spec("backup")]).unwrap();
    let chunks = &output.manifest.files[0].chunks;
    fetcher.fetch_chunk(chunks[0].id, &output.manifest).unwrap();
    assert_eq!(fetcher.source_ranking()[0], "primary");
    primary.delay.store(150, Ordering::Release);
    for chunk in chunks.iter().skip(1).take(12) {
        fetcher.fetch_chunk(chunk.id, &output.manifest).unwrap();
    }
    assert_eq!(fetcher.source_ranking()[0], "backup");
    assert!(backup.requests.load(Ordering::Acquire) > 1);
    let mut state = SourceHealth::new(2);
    state.rates = vec![100.0, 120.0];
    for i in 0..30 {
        state.observe(i % 2, if i % 2 == 0 { 100.0 } else { 120.0 });
    }
    assert_eq!(state.order[0], 0);
}

#[test]
fn timed_out_probe_remains_a_working_backup_and_single_source_skips_probe() {
    let (_build, output, pack) = fixture();
    let primary = Server::new(pack.clone(), 1, 0);
    let backup = Server::new(pack.clone(), 1, 3500);
    let fetcher =
        MultiSourceFetcher::new(vec![primary.spec("primary"), backup.spec("backup")]).unwrap();
    fetcher.probe(&output.manifest);
    assert_eq!(fetcher.source_ranking(), vec!["primary", "backup"]);
    primary.fail.store(true, Ordering::Release);
    fetcher
        .fetch_chunk(output.manifest.files[0].chunks[0].id, &output.manifest)
        .unwrap();
    assert!(backup.requests.load(Ordering::Acquire) >= 2);
    let single = Server::new(pack, 1, 0);
    let fetcher = MultiSourceFetcher::new(vec![single.spec("only")]).unwrap();
    fetcher
        .fetch_chunk(output.manifest.files[0].chunks[0].id, &output.manifest)
        .unwrap();
    assert_eq!(single.requests.load(Ordering::Acquire), 1);
    assert_eq!(fetcher.probe_bytes(), 0);
}
