use pagesift_compress::{compress, decompress, decompress_into};
use std::io::Write;
use std::process::{Command, Stdio};

fn random(n: usize) -> Vec<u8> {
    let mut state = 0x123456789abcdefu64;
    (0..n)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 32) as u8
        })
        .collect()
}

#[test]
fn long_distance_repeats() {
    let first = random(100_000);
    let data = first.repeat(4);
    for level in [3, 9, 10, 12, 15, 19] {
        let z = compress(&data, level);
        assert_eq!(decompress(&z, data.len()).unwrap(), data, "level {level}");
    }
}

#[test]
fn sizes_levels_and_raw_fallback() {
    for size in [0, 1, 4, 7, 8, 31, 1024, 131071, 131072, 131073, 400_000] {
        let mut data = random(size);
        // 不可压缩块夹着稀疏匹配，覆盖 raw 回退后的重复偏移状态。
        for i in (64..size.saturating_sub(32)).step_by(1009) {
            data.copy_within(i - 32..i - 16, i);
        }
        for level in [i32::MIN, 0, 1, 2, 3, 8, 9, 12, 19] {
            let z = compress(&data, level);
            assert_eq!(
                decompress(&z, size).unwrap(),
                data,
                "size {size} level {level}"
            );
        }
    }
    for level in [20, 50, i32::MAX] {
        let data = b"abc abc abc abc xyz abc abc abc";
        assert_eq!(
            decompress(&compress(data, level), data.len()).unwrap(),
            data
        );
    }
}

#[test]
fn concatenation_and_existing_output() {
    let a = b"alpha alpha alpha ".repeat(15000);
    let b = b"beta beta beta ".repeat(10000);
    let mut z = compress(&a, 9);
    z.extend_from_slice(&compress(&b, 19));
    let mut out = b"prefix".to_vec();
    let limit = out.len() + a.len() + b.len();
    decompress_into(&z, &mut out, limit).unwrap();
    assert_eq!(&out[6..6 + a.len()], a);
    assert_eq!(&out[6 + a.len()..], b);
}

#[test]
fn output_error_keeps_only_complete_blocks() {
    let data = b"alpha beta gamma delta epsilon\n".repeat(10_000);
    let z = compress(&data, 3);
    // 删除已知内容长度，让输出限制在块执行期间触发，而非提前在帧头拒绝。
    let mut unknown = z[..4].to_vec();
    unknown.extend_from_slice(&[0x04, 0x38]);
    unknown.extend_from_slice(&z[9..]);
    let prefix = b"prefix";
    for room in [0, 100, 32768, 131072 + 100] {
        let mut out = prefix.to_vec();
        assert_eq!(
            decompress_into(&unknown, &mut out, prefix.len() + room),
            Err(pagesift_compress::Error::TooLarge)
        );
        let completed = if room >= 131072 { 131072 } else { 0 };
        assert_eq!(&out[..prefix.len()], prefix);
        assert_eq!(&out[prefix.len()..], &data[..completed]);
    }
    assert_eq!(decompress(&unknown, data.len()).unwrap(), data);
}

fn official(args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut child = Command::new("zstd")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("启动 zstd");
    let mut stdin = child.stdin.take().unwrap();
    std::thread::scope(|scope| {
        let writer = scope.spawn(move || stdin.write_all(input));
        let output = child.wait_with_output().unwrap();
        writer.join().unwrap().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    })
}

#[test]
fn official_interoperability() {
    if Command::new("zstd").arg("--version").output().is_err() {
        eprintln!("未安装 zstd，跳过官方互通测试");
        return;
    }
    let mut data = random(100_000).repeat(3);
    data.extend_from_slice(&b"<div class=\"content\">hello world</div>\n".repeat(4000));
    for level in [0, 1, 3, 8, 9, 12, 19] {
        assert_eq!(
            official(&["-dcq"], &compress(&data, level)),
            data,
            "level {level}"
        );
    }
    for level in ["-1", "-3", "-9", "-19"] {
        let z = official(&[level, "-cq"], &data);
        assert_eq!(decompress(&z, data.len()).unwrap(), data, "{level}");
    }
}
