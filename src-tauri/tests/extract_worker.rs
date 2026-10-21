//! Talks to the real `argos --extract-worker` binary. Unit tests compile the
//! library with `cfg(test)` and never spawn that process.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use argos_lib::extract_host::{ExtractHost, ExtractOutcome, IoFail, WORKER_DID_NOT_EXIT};

fn worker_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_argos"))
}

fn write_frame(writer: &mut impl Write, payload: &[u8]) {
    writer
        .write_all(&(payload.len() as u32).to_le_bytes())
        .unwrap();
    writer.write_all(payload).unwrap();
    writer.flush().unwrap();
}

fn read_frame(reader: &mut impl Read) -> Vec<u8> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf).unwrap();
    let len = u32::from_le_bytes(len_buf) as usize;
    assert!(
        len < 8 * 1024 * 1024,
        "frame length {len} (header {:?})",
        std::str::from_utf8(&len_buf).unwrap_or("?")
    );
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).unwrap();
    buf
}

/// Type1 font with no Widths. pdf-extract prints "no widths and not core font" to stdout.
fn pdf_custom_font() -> Vec<u8> {
    let data = b"BT /F1 12 Tf 36 100 Td (HelloWidth) Tj ET\n";
    let stream = format!(
        "<< /Length {} >>\nstream\n{}endstream",
        data.len(),
        std::str::from_utf8(data).unwrap()
    );
    let objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_vec(),
        stream.into_bytes(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /CustomNoWidth >>".to_vec(),
    ];
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, obj) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend(obj);
        out.extend(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    out.extend(b"0000000000 65535 f \n");
    for off in offsets {
        out.extend(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

#[test]
fn worker_frames_survive_pdf_extract_stdout_warning() {
    let dir = std::env::temp_dir().join(format!(
        "argos-worker-frame-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let pdf = dir.join("widths.pdf");
    std::fs::write(&pdf, pdf_custom_font()).unwrap();

    let mut child = Command::new(worker_bin())
        .arg("--extract-worker")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();

    let ping = serde_json::to_vec(&serde_json::json!("Ping")).unwrap();
    write_frame(&mut stdin, &ping);
    let pong = read_frame(&mut stdout);
    assert_eq!(pong, br#""Pong""#);

    let req = serde_json::to_vec(&serde_json::json!({
        "Extract": {
            "path": pdf.to_string_lossy(),
            "max_bytes": 100_000_000u64
        }
    }))
    .unwrap();
    write_frame(&mut stdin, &req);
    let extracted = read_frame(&mut stdout);
    let value: serde_json::Value = serde_json::from_slice(&extracted)
        .unwrap_or_else(|err| panic!("frame was not json ({err}): {extracted:?}"));
    let pages = &value["Extracted"]["outcome"]["Text"]["pages"];
    let rendered = pages.to_string();
    assert!(
        rendered.contains("HelloWidth"),
        "pdf warning corrupted the frame or dropped the text: {value}"
    );

    write_frame(&mut stdin, &ping);
    let pong = read_frame(&mut stdout);
    assert_eq!(
        pong, br#""Pong""#,
        "stream misaligned after the pdf warning"
    );

    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success(), "{status}");
    let mut err_text = String::new();
    stderr.read_to_string(&mut err_text).unwrap();
    assert!(
        err_text.contains("no widths and not core font"),
        "expected the pdf-extract stdout warning on stderr, got {err_text:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn worker_timeout_kills_the_child() {
    let dir = std::env::temp_dir().join(format!("argos-worker-kill-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let blocked = dir.join("block.txt");
    std::fs::write(&blocked, "blocked body").unwrap();
    let ok = dir.join("ok.txt");
    std::fs::write(&ok, "契約の本文です。追加の文字。").unwrap();

    std::env::set_var("ARGOS_EXTRACT_WORKER", worker_bin());
    std::env::set_var("ARGOS_EXTRACT_BLOCK", "block.txt");
    std::env::set_var("ARGOS_EXTRACT_BLOCK_SECS", "30");
    let mut host = ExtractHost::new();
    let started = Instant::now();
    let err = host
        .extract(&blocked, Duration::from_millis(400), 100_000_000)
        .expect_err("blocked extract should time out");
    let elapsed = started.elapsed();
    assert!(
        matches!(err, IoFail::TimedOut),
        "expected timeout, got {err:?}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "kill wait took {elapsed:?}; the child sleep is 30s"
    );
    assert!(
        !WORKER_DID_NOT_EXIT.load(std::sync::atomic::Ordering::SeqCst),
        "killed worker was still alive"
    );

    std::env::remove_var("ARGOS_EXTRACT_BLOCK");
    std::env::remove_var("ARGOS_EXTRACT_BLOCK_SECS");
    let loaded = host
        .extract(&ok, Duration::from_secs(20), 100_000_000)
        .unwrap_or_else(|err| panic!("worker did not recover after kill: {err:?}"));
    match loaded.outcome {
        ExtractOutcome::Text { pages, .. } => {
            assert!(
                pages.iter().any(|page| page.contains("契約")),
                "{pages:?}"
            );
        }
        other => panic!("expected text, got {other:?}"),
    }
    drop(host);
    let _ = std::fs::remove_dir_all(&dir);
}
