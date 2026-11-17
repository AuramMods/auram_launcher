//! Windows Java 17's stock launcher converts UTF-16 arguments through the legacy
//! ANSI codepage. Use the verified runtime's own JLI in a UTF-8 manifested process.
//! A private stdin pipe carries arguments, including credentials, without files or
//! the Windows command-line length limit. macOS retains its first-thread launcher.
use anyhow::Result;
use std::path::Path;
use tokio::{
    io::AsyncWriteExt,
    process::{Child, Command},
};

pub fn command(java: &Path, arguments: &[String]) -> Result<(Command, Option<Vec<u8>>)> {
    #[cfg(windows)]
    {
        let helper = helper(java)?;
        let mut command = Command::new(helper);
        command.arg("--auram-java-pipe");
        Ok((command, Some(serde_json::to_vec(arguments)?)))
    }
    #[cfg(not(windows))]
    {
        let mut command = Command::new(java);
        command.args(arguments);
        Ok((command, None))
    }
}

pub async fn feed(child: &mut Child, input: Option<Vec<u8>>) -> Result<()> {
    if let Some(input) = input {
        use anyhow::Context;
        let mut stdin = child.stdin.take().context("Java argument pipe missing")?;
        stdin.write_all(&input).await?;
        stdin.shutdown().await?;
    }
    Ok(())
}

#[cfg(windows)]
pub fn helper(java: &Path) -> Result<std::path::PathBuf> {
    use anyhow::Context;
    use sha2::{Digest, Sha256};
    // Hash the actual executable so dev rebuilds cannot accidentally reuse a stale helper.
    let executable = std::env::current_exe()?;
    let hash = hex::encode(Sha256::digest(std::fs::read(&executable)?));
    let target = java
        .parent()
        .context("Java bin directory missing")?
        .join(format!("auram-java-{}.exe", &hash[..16]));
    if !target.exists() || hex::encode(Sha256::digest(std::fs::read(&target)?)) != hash {
        crate::storage::atomic_copy(&executable, &target)?;
    }
    Ok(target)
}

/// Runs before desktop initialization in the helper copy of this executable.
pub fn entry() {
    #[cfg(windows)]
    {
        let mode = std::env::args().nth(1).unwrap_or_default();
        if mode != "--auram-java-pipe" && mode != "--auram-java-cli" {
            return;
        }
        let result = (|| -> Result<i32> {
            let args = if mode == "--auram-java-pipe" {
                serde_json::from_reader(std::io::stdin().lock())?
            } else {
                std::env::args().skip(2).collect::<Vec<String>>()
            };
            launch(args)
        })();
        match result {
            Ok(code) => std::process::exit(code),
            Err(error) => {
                eprintln!("Unable to start managed Java: {error:#}");
                std::process::exit(1);
            }
        }
    }
}

#[cfg(windows)]
fn launch(args: Vec<String>) -> Result<i32> {
    use anyhow::Context;
    use std::ffi::{c_char, CString};
    #[repr(C)]
    struct StdArg {
        arg: *mut c_char,
        wildcard: u8,
    }
    type Launch = unsafe extern "system" fn(
        i32,
        *mut *mut c_char,
        i32,
        *const *const c_char,
        i32,
        *const *const c_char,
        *const c_char,
        *const c_char,
        *const c_char,
        *const c_char,
        u8,
        u8,
        u8,
        i32,
    ) -> i32;
    let exe = std::env::current_exe()?;
    let dll = exe
        .parent()
        .context("Java bin directory missing")?
        .join("jli.dll");
    let mut all = vec![exe.to_string_lossy().into_owned()];
    all.extend(args);
    let line = CString::new(
        all.iter()
            .map(|s| quote_windows(s))
            .collect::<Vec<_>>()
            .join(" "),
    )?;
    // SAFETY: This pinned Java 17 JLI ABI is declared in OpenJDK's java.h and
    // jli_util.h. The library and its argument storage remain live until exit.
    unsafe {
        let library = libloading::Library::new(dll)?;
        let init = library.get::<unsafe extern "system" fn(u8, u8)>(b"JLI_InitArgProcessing\0")?;
        let parse = library.get::<unsafe extern "system" fn(*mut c_char)>(b"JLI_CmdToArgs\0")?;
        let count = library.get::<unsafe extern "system" fn() -> i32>(b"JLI_GetStdArgc\0")?;
        let values =
            library.get::<unsafe extern "system" fn() -> *mut StdArg>(b"JLI_GetStdArgs\0")?;
        let launch = library.get::<Launch>(b"JLI_Launch\0")?;
        init(0, 0);
        let mut line = line.into_bytes_with_nul();
        parse(line.as_mut_ptr().cast());
        let count = count();
        let mut argv = std::slice::from_raw_parts(values(), count as usize)
            .iter()
            .map(|v| v.arg)
            .collect::<Vec<_>>();
        argv.push(std::ptr::null_mut());
        Ok(launch(
            count,
            argv.as_mut_ptr(),
            0,
            std::ptr::null(),
            0,
            std::ptr::null(),
            c"17".as_ptr(),
            c"17".as_ptr(),
            c"java".as_ptr(),
            c"java".as_ptr(),
            0,
            1,
            0,
            0,
        ))
    }
}

#[cfg(any(windows, test))]
fn quote_windows(value: &str) -> String {
    let mut result = String::from("\"");
    let mut slashes = 0;
    for character in value.chars() {
        if character == '\\' {
            slashes += 1;
            continue;
        }
        if character == '"' {
            result.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
        } else {
            result.extend(std::iter::repeat_n('\\', slashes));
        }
        slashes = 0;
        result.push(character);
    }
    result.extend(std::iter::repeat_n('\\', slashes * 2));
    result.push('"');
    result
}

#[cfg(test)]
mod tests {
    #[test]
    fn quotes_windows_arguments_including_trailing_backslashes() {
        assert_eq!(
            super::quote_windows("C:\\World 世界\\"),
            "\"C:\\World 世界\\\\\""
        );
        assert_eq!(super::quote_windows("a\\\"b"), "\"a\\\\\\\"b\"");
        assert_eq!(super::quote_windows(""), "\"\"");
    }
}
