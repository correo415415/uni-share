//! End-to-end LAN transfer over loopback: real TLS 1.3 (pinned), real HTTP/2,
//! folder hierarchy, BLAKE3 verification, duplicate renaming, rejection.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use uni_share::fsutil::collect_files;
use uni_share::lan::client::{Sender, build_manifest, hash_all};
use uni_share::lan::server::{Decision, ServerOptions, start};
use uni_share::lan::tls::Identity;

fn make_tree(root: &std::path::Path) {
    std::fs::create_dir_all(root.join("proj/sub/deep")).unwrap();
    std::fs::write(root.join("proj/a.txt"), b"hello world").unwrap();
    let big: Vec<u8> = (0..3_500_000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
    std::fs::write(root.join("proj/sub/big.bin"), &big).unwrap();
    std::fs::write(root.join("proj/sub/deep/empty.txt"), b"").unwrap();
}

async fn spawn_receiver(dest: PathBuf, pin: Option<&str>, force: bool) -> (uni_share::lan::server::ServerHandle, Identity) {
    let id = Identity::generate("Receiver").unwrap();
    let h = start(
        id.clone(),
        ServerOptions {
            device_name: "Receiver".into(),
            port: 0,
            pin: pin.map(str::to_string),
            force_overwrite: force,
            dest_dir: dest,
            rate_limit_mbps: 0,
            state_dir: None,
        },
    )
    .await
    .unwrap();
    (h, id)
}

#[tokio::test]
async fn folder_transfer_end_to_end_with_pinning() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    make_tree(src.path());
    let (mut server, id) = spawn_receiver(dst.path().to_path_buf(), None, false).await;
    let port = server.addr.port();
    let dest_dir = dst.path().to_path_buf();

    // Auto-accepting "UI".
    tokio::spawn(async move {
        while let Some(offer) = server.offers.recv().await {
            assert_eq!(offer.manifest.files.len(), 3);
            let _ = offer.decision.send(Decision::Accept { dest_dir: dest_dir.clone() });
        }
    });

    let files = collect_files(&src.path().join("proj")).unwrap();
    let hashes = hash_all(&files, |_| {}).await.unwrap();
    let manifest = build_manifest("Sender", "abc", "proj", &files, &hashes);
    let sender = Sender::new("127.0.0.1".parse().unwrap(), port, Some(&id.fingerprint), None).unwrap();
    let info = sender.info().await.unwrap();
    assert_eq!(info.name, "Receiver");
    assert_eq!(info.fingerprint, id.fingerprint);

    let sent = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let s2 = sent.clone();
    let progress: uni_share::lan::client::ProgressFn = Arc::new(move |n, _| {
        s2.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
    });
    let report = sender.send(&manifest, &files, &hashes, progress, Duration::from_secs(10), 0).await.unwrap();
    assert_eq!(report.files, 3);
    assert_eq!(sent.load(std::sync::atomic::Ordering::Relaxed), manifest.total_size);

    // Hierarchy preserved + content identical.
    assert_eq!(std::fs::read(dst.path().join("proj/a.txt")).unwrap(), b"hello world");
    assert_eq!(
        std::fs::read(dst.path().join("proj/sub/big.bin")).unwrap(),
        std::fs::read(src.path().join("proj/sub/big.bin")).unwrap()
    );
    assert!(dst.path().join("proj/sub/deep/empty.txt").exists());
    assert!(!dst.path().join("proj/a.txt.part").exists());

    // Second send → duplicates renamed "(1)".
    let report2 = sender.send(&manifest, &files, &hashes, Arc::new(|_, _| {}), Duration::from_secs(10), 0).await.unwrap();
    assert_eq!(report2.files, 3);
    assert!(dst.path().join("proj/a (1).txt").exists());
}

#[tokio::test]
async fn wrong_fingerprint_is_rejected_by_tls() {
    let dst = tempfile::tempdir().unwrap();
    let (server, _id) = spawn_receiver(dst.path().to_path_buf(), None, false).await;
    let other = Identity::generate("Evil").unwrap();
    let sender = Sender::new("127.0.0.1".parse().unwrap(), server.addr.port(), Some(&other.fingerprint), None).unwrap();
    let err = sender.info().await.expect_err("must fail");
    let msg = format!("{err:#}");
    assert!(msg.contains("fingerprint") || msg.contains("certificate") || msg.contains("tls"), "{msg}");
}

#[tokio::test]
async fn pin_required_and_rejection() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    std::fs::write(src.path().join("f.txt"), b"data").unwrap();
    let (mut server, id) = spawn_receiver(dst.path().to_path_buf(), Some("1234"), false).await;
    let port = server.addr.port();
    tokio::spawn(async move {
        while let Some(offer) = server.offers.recv().await {
            let _ = offer.decision.send(Decision::Reject { reason: "nope".into() });
        }
    });
    let files = collect_files(&src.path().join("f.txt")).unwrap();
    let hashes = hash_all(&files, |_| {}).await.unwrap();
    let manifest = build_manifest("S", "", "f.txt", &files, &hashes);

    // No PIN → unauthorized.
    let no_pin = Sender::new("127.0.0.1".parse().unwrap(), port, Some(&id.fingerprint), None).unwrap();
    let e = no_pin.offer(&manifest, Duration::from_secs(5)).await.expect_err("must fail");
    assert!(format!("{e}").contains("PIN"));

    // Right PIN but receiver rejects.
    let with_pin = Sender::new("127.0.0.1".parse().unwrap(), port, Some(&id.fingerprint), Some("1234".into())).unwrap();
    let e = with_pin.offer(&manifest, Duration::from_secs(5)).await.expect_err("must fail");
    assert!(format!("{e}").contains("rejected"));
}

#[tokio::test]
async fn hash_mismatch_is_detected_and_file_discarded() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    std::fs::write(src.path().join("f.txt"), b"correct data").unwrap();
    let (mut server, id) = spawn_receiver(dst.path().to_path_buf(), None, false).await;
    let port = server.addr.port();
    let dest_dir = dst.path().to_path_buf();
    tokio::spawn(async move {
        while let Some(offer) = server.offers.recv().await {
            let _ = offer.decision.send(Decision::Accept { dest_dir: dest_dir.clone() });
        }
    });
    let files = collect_files(&src.path().join("f.txt")).unwrap();
    // Lie about the hash → receiver must answer hash_mismatch each time → sender gives up after retries.
    let bad = vec!["00".repeat(32)];
    let manifest = build_manifest("S", "", "f.txt", &files, &bad);
    let sender = Sender::new("127.0.0.1".parse().unwrap(), port, Some(&id.fingerprint), None).unwrap();
    let err = sender.send(&manifest, &files, &bad, Arc::new(|_, _| {}), Duration::from_secs(5), 0).await.expect_err("must fail");
    assert!(format!("{err}").contains("integrity"), "{err}");
    assert!(!dst.path().join("f.txt").exists());
    assert!(!dst.path().join("f.txt.part").exists());
}

/// A reception interrupted in one receiver process resumes in the next one:
/// the resume record remembers destination and per-file state, and the `.part`
/// prefix already on disk is not transferred again.
#[tokio::test]
async fn resume_across_receiver_restarts() {
    use uni_share::lan::protocol::UploadResult;

    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    make_tree(src.path());
    let files = collect_files(&src.path().join("proj")).unwrap();
    let hashes = hash_all(&files, |_| {}).await.unwrap();
    let manifest = build_manifest("Sender", "sender-fp-1", "proj", &files, &hashes);
    let big_idx = files.iter().position(|f| f.rel_path.ends_with("big.bin")).unwrap();
    let small_idx = files.iter().position(|f| f.rel_path.ends_with("a.txt")).unwrap();

    let id = Identity::generate("Receiver").unwrap();
    let opts = |dest: PathBuf| ServerOptions {
        device_name: "Receiver".into(),
        port: 0,
        pin: None,
        force_overwrite: false,
        dest_dir: dest,
        rate_limit_mbps: 0,
        state_dir: Some(state.path().to_path_buf()),
    };

    // ── run 1: accept, receive only a.txt, then "crash" ──
    let mut server = start(id.clone(), opts(dst.path().to_path_buf())).await.unwrap();
    let port = server.addr.port();
    let dest1 = dst.path().to_path_buf();
    tokio::spawn(async move {
        if let Some(offer) = server.offers.recv().await {
            assert!(offer.resume.is_none(), "first run must not offer a resume");
            let _ = offer.decision.send(Decision::Accept { dest_dir: dest1 });
        }
        // Keep the server alive until the test drops the handle.
        std::future::pending::<()>().await;
    });
    let sender = Sender::new("127.0.0.1".parse().unwrap(), port, Some(&id.fingerprint), None).unwrap();
    let tid = sender.offer(&manifest, Duration::from_secs(5)).await.unwrap();
    let progress: uni_share::lan::client::ProgressFn = Arc::new(|_, _| {});
    let r = sender.upload_file(&tid, small_idx, &files[small_idx], &hashes[small_idx], &progress, 0).await.unwrap();
    assert!(matches!(r, UploadResult::Ok { .. }));
    // Simulate an interrupted big.bin: the receiver had written a 1 MiB prefix.
    let big_src = std::fs::read(&files[big_idx].abs_path).unwrap();
    std::fs::create_dir_all(dst.path().join("proj/sub")).unwrap();
    std::fs::write(dst.path().join("proj/sub/big.bin.part"), &big_src[..1_048_576]).unwrap();
    let rec_path = state.path().join("transfers");
    assert_eq!(std::fs::read_dir(&rec_path).unwrap().count(), 1, "one resume record written");
    // (server task is aborted when the runtime drops it at the end; we just start another one)

    // ── run 2: same identity + state dir, fresh process, offer again ──
    let other = tempfile::tempdir().unwrap(); // the user picks another folder: must be ignored
    let mut server2 = start(id.clone(), opts(other.path().to_path_buf())).await.unwrap();
    let port2 = server2.addr.port();
    let (rtx, rrx) = tokio::sync::oneshot::channel();
    let other_dir = other.path().to_path_buf();
    tokio::spawn(async move {
        if let Some(offer) = server2.offers.recv().await {
            let _ = rtx.send(offer.resume.clone());
            let _ = offer.decision.send(Decision::Accept { dest_dir: other_dir });
        }
        std::future::pending::<()>().await;
    });
    let sender2 = Sender::new("127.0.0.1".parse().unwrap(), port2, Some(&id.fingerprint), None).unwrap();
    let sent = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let s2 = sent.clone();
    let progress2: uni_share::lan::client::ProgressFn = Arc::new(move |n, _| {
        s2.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
    });
    let report = sender2.send(&manifest, &files, &hashes, progress2, Duration::from_secs(5), 0).await.unwrap();
    assert_eq!(report.files, 3);

    let resume = rrx.await.unwrap().expect("second run must detect the interrupted transfer");
    assert_eq!(resume.dest_dir, dst.path());
    assert_eq!(resume.files_done, 1);
    assert_eq!(resume.bytes_done, files[small_idx].size + 1_048_576);

    // Data landed in the ORIGINAL destination, complete and verified …
    assert_eq!(std::fs::read(dst.path().join("proj/sub/big.bin")).unwrap(), big_src);
    assert_eq!(std::fs::read(dst.path().join("proj/a.txt")).unwrap(), b"hello world");
    assert!(!dst.path().join("proj/sub/big.bin.part").exists());
    assert_eq!(std::fs::read_dir(other.path()).unwrap().count(), 0, "user-chosen folder ignored on resume");
    // … only the missing bytes travelled (a.txt skipped, big.bin from 1 MiB) …
    let expected = manifest.total_size - files[small_idx].size - 1_048_576;
    assert_eq!(sent.load(std::sync::atomic::Ordering::Relaxed), expected);
    // … and the record is gone.
    assert_eq!(std::fs::read_dir(&rec_path).unwrap().count(), 0);
}
