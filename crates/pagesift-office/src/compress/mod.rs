//! 压缩格式：LZMA、LZMA2、xz、bzip2 和 7z 容器。都是自己写的解码器，只读。
//! 测试样本在 `tests/data/compress/`，由 xz、bzip2、7z 这些标准工具生成。

pub(crate) mod bzip2;
mod lzma;
pub(crate) mod sevenz;
pub(crate) mod xz;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error;

    const TEXT: &[u8] = include_bytes!("../../tests/data/compress/text.txt");

    #[test]
    fn xz_matches_the_standard_tool() {
        let cases: [&[u8]; 3] = [
            include_bytes!("../../tests/data/compress/text.xz"),
            include_bytes!("../../tests/data/compress/text.crc32.xz"),
            include_bytes!("../../tests/data/compress/two-streams.xz"),
        ];
        assert_eq!(xz::decode(cases[0], 1 << 20).unwrap(), TEXT);
        assert_eq!(xz::decode(cases[1], 1 << 20).unwrap(), TEXT);
        assert_eq!(
            xz::decode(cases[2], 1 << 20).unwrap(),
            [TEXT, TEXT].concat()
        );
        assert_eq!(
            xz::decode(cases[0], 10).unwrap_err(),
            Error::TooLarge("decompressed size")
        );
    }

    #[test]
    fn bzip2_matches_the_standard_tool() {
        let one = include_bytes!("../../tests/data/compress/text.bz2");
        let two = include_bytes!("../../tests/data/compress/two-streams.bz2");
        assert_eq!(bzip2::decode(one, 1 << 20).unwrap(), TEXT);
        assert_eq!(bzip2::decode(two, 1 << 20).unwrap(), [TEXT, TEXT].concat());
        assert_eq!(
            bzip2::decode(one, 10).unwrap_err(),
            Error::TooLarge("decompressed size")
        );
        // 连续相同字节会触发第一层游程
        let runs = include_bytes!("../../tests/data/compress/runs.bz2");
        let expected: Vec<u8> = [vec![b'a'; 1000], vec![b'b'; 3], vec![b'c'; 300]].concat();
        assert_eq!(bzip2::decode(runs, 1 << 20).unwrap(), expected);
    }

    #[test]
    fn sevenz_matches_the_standard_tool() {
        let lzma2 = include_bytes!("../../tests/data/compress/files.7z");
        let lzma1 = include_bytes!("../../tests/data/compress/files-lzma.7z");
        let copy = include_bytes!("../../tests/data/compress/files-copy.7z");
        for (name, archive) in [
            ("lzma2", &lzma2[..]),
            ("lzma", &lzma1[..]),
            ("copy", &copy[..]),
        ] {
            let mut files = sevenz::decode(archive, 1 << 20).unwrap();
            files.sort();
            let names: Vec<&str> = files.iter().map(|f| f.0.as_str()).collect();
            assert_eq!(names, ["a.txt", "dir/b.txt"], "{name}");
            assert_eq!(files[0].1, TEXT, "{name}");
            assert_eq!(files[1].1, b"second file\n", "{name}");
        }
    }

    #[test]
    fn corrupt_and_truncated_input_never_panics() {
        let samples: [&[u8]; 3] = [
            include_bytes!("../../tests/data/compress/text.xz"),
            include_bytes!("../../tests/data/compress/text.bz2"),
            include_bytes!("../../tests/data/compress/files.7z"),
        ];
        for s in samples {
            for cut in (0..s.len()).step_by(7) {
                let _ = xz::decode(&s[..cut], 1 << 16);
                let _ = bzip2::decode(&s[..cut], 1 << 16);
                let _ = sevenz::decode(&s[..cut], 1 << 16);
            }
            for i in (0..s.len()).step_by(3) {
                let mut bad = s.to_vec();
                bad[i] ^= 0x55;
                let _ = xz::decode(&bad, 1 << 16);
                let _ = bzip2::decode(&bad, 1 << 16);
                let _ = sevenz::decode(&bad, 1 << 16);
            }
        }
    }
}
