//! 写 stdout 的 helper：下游提前关闭管道（`pagesift ... | head`）时安静退出，
//! 不让 `println!` 的 EPIPE 变成 panic。

use std::fmt;
use std::io::{self, Write};

/// Broken pipe 的常规退出码：128 + SIGPIPE(13)。
const SIGPIPE_EXIT: i32 = 141;

pub(crate) fn write_stdout(args: fmt::Arguments<'_>, newline: bool) {
    let mut out = io::stdout().lock();
    let result = out
        .write_fmt(args)
        .and_then(|()| if newline { out.write_all(b"\n") } else { Ok(()) })
        .and_then(|()| out.flush());
    if let Err(error) = result {
        if error.kind() == io::ErrorKind::BrokenPipe {
            // 读端已经走了，继续写也没有意义；按 SIGPIPE 的惯例退出。
            std::process::exit(SIGPIPE_EXIT);
        }
        let _ = writeln!(io::stderr(), "error: failed to write stdout: {error}");
        std::process::exit(1);
    }
}

pub(crate) fn write_stderr(args: fmt::Arguments<'_>, newline: bool) {
    let mut out = io::stderr().lock();
    let result = out
        .write_fmt(args)
        .and_then(|()| if newline { out.write_all(b"\n") } else { Ok(()) })
        .and_then(|()| out.flush());
    if let Err(error) = result
        && error.kind() == io::ErrorKind::BrokenPipe
    {
        // stderr 和 stdout 合并进同一个管道时（2>&1 | head）也会遇到 EPIPE。
        std::process::exit(SIGPIPE_EXIT);
    }
}

/// 覆盖 `std::println!`：CLI 的每一行都走 [`write_stdout`]。
macro_rules! println {
    () => {
        $crate::output::write_stdout(format_args!(""), true)
    };
    ($($arg:tt)*) => {
        $crate::output::write_stdout(format_args!($($arg)*), true)
    };
}

/// 覆盖 `std::print!`：Markdown、JSON 等无换行输出同样处理 EPIPE。
macro_rules! print {
    ($($arg:tt)*) => {
        $crate::output::write_stdout(format_args!($($arg)*), false)
    };
}

/// 覆盖 `std::eprintln!`：和 stdout 合并进管道时安静退出。
macro_rules! eprintln {
    () => {
        $crate::output::write_stderr(format_args!(""), true)
    };
    ($($arg:tt)*) => {
        $crate::output::write_stderr(format_args!($($arg)*), true)
    };
}

#[cfg(test)]
mod tests {
    use super::SIGPIPE_EXIT;

    #[test]
    fn sigpipe_exit_matches_shell_convention() {
        assert_eq!(SIGPIPE_EXIT, 141);
    }
}
