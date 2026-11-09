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
                    socket.set_nonblocking(false).unwrap();
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
    fixture_with_size(4 * 1024 * 1024)
}

fn fixture_with_size(size: usize) -> (tempfile::TempDir, vesper_format::BuildOutput, Vec<u8>) {
    let input = tempfile::tempdir().unwrap();
    let mut x = 17u64;
    let data: Vec<u8> = (0..size)
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
    let (_build, output, pack) = fixture_with_size(32 * 1024 * 1024);
    let slow = Server::new(pack.clone(), 1000, 0);
    // Distort the fast source's probe; useful early samples must correct it.
    let fast = Server::new(pack, 0, 2000);
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
    assert!(fetcher.sampling_bytes() > 0);
    let state = fetcher.health.lock().unwrap();
    assert!(state.sampled_bytes <= state.scheduled_bytes / 20);
    assert_eq!(fetcher.hedged_bytes(), 0);
    assert!(total <= output.manifest.packs[0].size + fetcher.probe_bytes());
}

#[test]
fn slowing_primary_switches_without_oscillation() {
    let (_build, output, pack) = fixture();
    let primary = Server::new(pack.clone(), 0, 0);
    let backup = Server::new(pack, 0, 2000);
    let fetcher =
        MultiSourceFetcher::new(vec![primary.spec("primary"), backup.spec("backup")]).unwrap();
    let chunks = &output.manifest.files[0].chunks;
    fetcher.fetch_chunk(chunks[0].id, &output.manifest).unwrap();
    assert_eq!(fetcher.source_ranking()[0], "primary");
    primary.delay.store(1000, Ordering::Release);
    let deadline = Instant::now() + Duration::from_secs(120);
    for chunk in chunks.iter().skip(1).cycle().take(240) {
        fetcher.fetch_chunk(chunk.id, &output.manifest).unwrap();
        if fetcher.source_ranking()[0] == "backup" || Instant::now() >= deadline {
            break;
        }
    }
    assert_eq!(fetcher.source_ranking()[0], "backup");
    assert!(backup.requests.load(Ordering::Acquire) > 1);
    let state = fetcher.health.lock().unwrap();
    assert!(state.sampled_bytes > 0 && state.sampled_bytes <= state.scheduled_bytes / 20);
    drop(state);
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

#[test]
fn stale_or_probe_only_rates_cannot_trigger_switch_and_sampling_is_bounded() {
    let mut state = SourceHealth::new(3);
    let now = Instant::now();
    state.rates = vec![1.0, 1_000_000.0, 2_000_000.0];
    for _ in 0..10 {
        state.observe(0, 1.0, now);
    }
    assert_eq!(state.order[0], 0);
    // Neither a stale primary nor a stale backup can supply switch evidence.
    state.measured[0] = Some(now - Duration::from_secs(31));
    state.observe(1, 100.0, now);
    assert_eq!(state.order[0], 0);
    for _ in 0..10 {
        state.observe(0, 1.0, now + Duration::from_secs(31));
    }
    assert_eq!(state.order[0], 0);
    let mut seen = [false; 3];
    for bytes in (1..3000).map(|n| (n * 7919 % 1_000_000 + 1) as u64) {
        if let Some(index) = state.sample_source(bytes) {
            seen[index] = true;
        }
        assert!(state.sampled_bytes <= state.scheduled_bytes / 20);
    }
    assert!(seen[1] && seen[2]);
}

#[test]
fn probe_ranking_uses_initial_order_for_ties_and_failed_probes_sort_last() {
    let mut state = SourceHealth::new(4);
    state.order = vec![2, 1, 0, 3];
    state.rank_probes(vec![0.0, 100.0, 95.0, 500.0]);
    assert_eq!(state.order, vec![3, 2, 1, 0]);
    assert!(state.measured.iter().all(Option::is_none));
}

#[test]
fn noisy_probe_is_corrected_by_first_fresh_sample_even_during_cooldown() {
    let now = Instant::now();
    let mut state = SourceHealth::new(2);
    state.rank_probes(vec![1000.0, 1.0]);
    state.cooldown = 8;
    state.observe(0, 100.0, now);
    // Probe-only backup evidence cannot switch, regardless of the probe rate.
    assert_eq!(state.order[0], 0);
    for _ in 0..19 {
        assert_eq!(state.sample_source(1024), None);
    }
    assert_eq!(state.sample_source(1024), Some(1));
    state.observe(1, 500.0, now + Duration::from_secs(1));
    assert_eq!(state.order[0], 1);
    assert_eq!(state.cooldown, 8);
    assert_eq!(state.sampled_bytes, state.scheduled_bytes / 20);
}

#[test]
fn close_content_rates_do_not_oscillate() {
    let now = Instant::now();
    let mut state = SourceHealth::new(2);
    state.rank_probes(vec![101.0, 100.0]);
    for n in 0..100 {
        let time = now + Duration::from_millis(n * 100);
        state.observe(0, if n % 2 == 0 { 100.0 } else { 120.0 }, time);
        state.observe(1, if n % 2 == 0 { 120.0 } else { 100.0 }, time);
        assert_eq!(state.order[0], 0);
    }
}

#[test]
fn ordinary_switch_requires_three_votes_and_eight_completion_cooldown() {
    let now = Instant::now();
    let mut state = SourceHealth::new(2);
    state.observe(0, 100.0, now);
    state.observe(1, 200.0, now);
    assert_eq!(state.order[0], 0);
    state.observe(1, 200.0, now);
    assert_eq!(state.order[0], 0);
    state.observe(1, 200.0, now);
    assert_eq!(state.order[0], 1);
    assert_eq!(state.cooldown, 8);
    for _ in 0..8 {
        state.observe(0, 400.0, now);
        assert_eq!(state.order[0], 1);
    }
    for _ in 0..2 {
        state.observe(0, 400.0, now);
        assert_eq!(state.order[0], 1);
    }
    state.observe(0, 400.0, now);
    assert_eq!(state.order[0], 0);
}

#[test]
fn stale_measurement_resets_votes_and_ewma_before_new_evidence() {
    let now = Instant::now();
    let mut state = SourceHealth::new(2);
    state.observe(0, 100.0, now);
    state.observe(1, 200.0, now);
    assert_eq!(state.votes, 1);
    let later = now + Duration::from_secs(31);
    state.observe(0, 100.0, later);
    assert_eq!(state.votes, 0);
    state.observe(1, 120.0, later);
    assert_eq!(state.rates[1], 120.0);
    assert_eq!(state.order[0], 0);
}

#[test]
fn early_sampling_preserves_every_frame_without_overlap() {
    let (_build, output, _) = fixture();
    let ids: Vec<_> = output.manifest.files[0]
        .chunks
        .iter()
        .map(|c| c.id)
        .collect();
    let plans = plan_ranges_for_manifest(&output.manifest, &ids).unwrap();
    let original: Vec<_> = plans
        .iter()
        .flat_map(|p| p.chunks.iter().copied())
        .collect();
    let split = early_sampling_ranges(plans);
    let actual: Vec<_> = split
        .iter()
        .flat_map(|p| p.chunks.iter().copied())
        .collect();
    assert_eq!(original, actual);
    assert!(split.len() > 20);
    for pair in split.windows(2) {
        if pair[0].pack_id == pair[1].pack_id {
            assert!(pair[0].end < pair[1].start);
        }
    }
    assert!(early_sampling_ranges(Vec::new()).is_empty());
}
