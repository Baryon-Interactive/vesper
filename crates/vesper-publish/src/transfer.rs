//! Bounded file transfers with connection and progress-based idle deadlines.
use anyhow::{Context, Result};
use reqwest::{header::HeaderMap, StatusCode};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub(crate) const IDLE: Duration = Duration::from_secs(60);
pub(crate) fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?)
}
pub(crate) fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        // reqwest read_timeout also covers waiting for upload response headers,
        // so it would impose an upload duration limit. send_file owns that timer.
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}

/// Each poll supplies at most 64 KiB; the watchdog cancels stalled socket writes
/// as well as waits for response headers. There is no total transfer deadline.
pub(crate) async fn send_file(
    builder: reqwest::RequestBuilder,
    path: Option<&Path>,
    idle: Duration,
) -> Result<(StatusCode, HeaderMap)> {
    let progress = Arc::new(Mutex::new(Instant::now()));
    let builder = if let Some(path) = path {
        let file = tokio::fs::File::open(path).await?;
        let size = file.metadata().await?.len();
        let tick = progress.clone();
        let stream = futures_util::stream::try_unfold(file, move |mut file| {
            let tick = tick.clone();
            async move {
                let mut buffer = vec![0u8; 64 * 1024];
                let n = file.read(&mut buffer).await?;
                *tick.lock().unwrap() = Instant::now();
                if n == 0 {
                    Ok::<_, std::io::Error>(None)
                } else {
                    buffer.truncate(n);
                    Ok(Some((buffer, file)))
                }
            }
        });
        builder
            .header(reqwest::header::CONTENT_LENGTH, size)
            .body(reqwest::Body::wrap_stream(stream))
    } else {
        builder
    };
    let send = builder.send();
    tokio::pin!(send);
    loop {
        let remaining = idle.saturating_sub(progress.lock().unwrap().elapsed());
        let pause = tokio::time::sleep(remaining);
        tokio::pin!(pause);
        match futures_util::future::select(&mut send, pause).await {
            futures_util::future::Either::Left((response, _)) => {
                let response = response.context("file transfer transport failed")?;
                return Ok((response.status(), response.headers().clone()));
            }
            futures_util::future::Either::Right(_) => {
                anyhow::ensure!(
                    progress.lock().unwrap().elapsed() < idle,
                    "file transfer idle timeout"
                );
            }
        }
    }
}

pub(crate) fn download(url: &str, path: &Path, size: u64) -> Result<()> {
    download_with_idle(url, path, size, IDLE)
}

fn download_with_idle(url: &str, path: &Path, size: u64, idle: Duration) -> Result<()> {
    runtime()?.block_on(async {
        // Downloads may follow public asset redirects without publishing credentials.
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .read_timeout(idle)
            .build()?;
        let mut response = tokio::time::timeout(
            idle,
            client
                .get(url)
                .header(
                    reqwest::header::RANGE,
                    format!("bytes=0-{}", size.saturating_sub(1)),
                )
                .send(),
        )
        .await??
        .error_for_status()?;
        let mut file = tokio::fs::File::create(path).await?;
        let mut received = 0u64;
        while let Some(bytes) = tokio::time::timeout(idle, response.chunk()).await?? {
            received = received.saturating_add(bytes.len() as u64);
            anyhow::ensure!(received <= size, "asset exceeds expected size");
            tokio::time::timeout(idle, file.write_all(&bytes)).await??;
        }
        file.flush().await?;
        anyhow::ensure!(received == size, "asset length/hash mismatch");
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    #[test]
    fn slow_progress_download_has_no_total_deadline_and_checks_length() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).unwrap();
                header.push(byte[0]);
            }
            socket.write_all(b"HTTP/1.1 206 Partial Content\r\nContent-Length: 128\r\nConnection: close\r\n\r\n").unwrap();
            for _ in 0..128 {
                socket.write_all(b"x").unwrap();
                thread::sleep(Duration::from_millis(20));
            }
        });
        let staging = tempfile::tempdir().unwrap();
        let path = staging.path().join("object");
        let started = Instant::now();
        download_with_idle(&url, &path, 128, Duration::from_secs(2)).unwrap();
        assert!(started.elapsed() > Duration::from_secs(2));
        assert_eq!(std::fs::read(path).unwrap(), vec![b'x'; 128]);
        server.join().unwrap();
    }

    #[test]
    fn progressing_file_upload_can_exceed_idle_interval() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let size = 32 * 1024 * 1024;
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(15)))
                .unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).unwrap();
                header.push(byte[0]);
            }
            let mut received = 0;
            let mut buffer = [0u8; 64 * 1024];
            while received < size {
                let n = socket.read(&mut buffer).unwrap();
                assert!(n > 0);
                assert!(buffer[..n].iter().all(|b| *b == 0));
                received += n;
                thread::sleep(Duration::from_millis(10));
            }
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .unwrap();
        });
        let staging = tempfile::tempdir().unwrap();
        let path = staging.path().join("large");
        std::fs::File::create(&path)
            .unwrap()
            .set_len(size as u64)
            .unwrap();
        let started = Instant::now();
        runtime().unwrap().block_on(async {
            let result = send_file(
                client().unwrap().put(url),
                Some(&path),
                Duration::from_secs(3),
            )
            .await
            .unwrap();
            assert!(result.0.is_success());
        });
        assert!(started.elapsed() > Duration::from_secs(3));
        server.join().unwrap();
    }

    #[test]
    fn stalled_file_upload_is_cancelled_by_write_idle_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (done, stop) = std::sync::mpsc::channel();
        let server = thread::spawn(move || {
            let (_socket, _) = listener.accept().unwrap();
            // The server never consumes the body or sends headers.
            stop.recv_timeout(Duration::from_secs(15)).unwrap();
        });
        let staging = tempfile::tempdir().unwrap();
        let path = staging.path().join("large");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(64 * 1024 * 1024).unwrap();
        let error = runtime()
            .unwrap()
            .block_on(async {
                send_file(
                    client().unwrap().put(url),
                    Some(&path),
                    Duration::from_millis(500),
                )
                .await
            })
            .unwrap_err();
        assert!(error.to_string().contains("idle timeout"));
        done.send(()).unwrap();
        server.join().unwrap();
    }
}
