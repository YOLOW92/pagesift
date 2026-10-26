//! CLI output should behave like a normal Unix filter when the reader goes away.

use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn closed_stdout_pipe_exits_141_without_panic() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("big.html");
    let mut file = std::fs::File::create(&input).unwrap();
    file.write_all(b"<article><h1>Big</h1>").unwrap();
    for _ in 0..20_000 {
        file.write_all(b"<p>The quick brown fox jumps over the lazy dog, again and again.</p>\n")
            .unwrap();
    }
    file.write_all(b"</article>").unwrap();
    drop(file);

    let mut child = Command::new(env!("CARGO_BIN_EXE_pagesift"))
        .arg("convert")
        .arg(&input)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // `pagesift convert big.html | head -1`: the reader disappears immediately.
    drop(child.stdout.take());
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(141), "stderr: {stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
    assert!(stderr.is_empty(), "{stderr}");
}
