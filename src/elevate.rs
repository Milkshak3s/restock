//! Running installers with administrator rights when Restock itself has none
//! (debug builds, or a release build started without elevation).

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::{io, mem, ptr};

use anyhow::{Result, bail};
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, HANDLE};
use windows_sys::Win32::Security::{
    GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, INFINITE, OpenProcessToken, WaitForSingleObject,
};
use windows_sys::Win32::UI::Shell::{
    SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

pub fn is_elevated() -> bool {
    // SAFETY: the token handle is checked before use and always closed; the
    // output buffer is a TOKEN_ELEVATION of the size we pass.
    unsafe {
        let mut token: HANDLE = ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            (&raw mut elevation).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        );
        CloseHandle(token);
        ok != 0 && elevation.TokenIsElevated != 0
    }
}

/// Starts `program` through a UAC prompt, waits for it, and returns its exit code.
pub fn run_elevated<'a>(program: &OsStr, args: impl Iterator<Item = &'a OsStr>) -> Result<i32> {
    let params: Vec<String> = args.map(quote_arg).collect();
    let wide = |s: &OsStr| s.encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    let verb = wide("runas".as_ref());
    let file = wide(program);
    let params = wide(params.join(" ").as_ref());

    // SAFETY: every string pointer stays alive (owned Vecs above) for the
    // call, and the process handle is only used after a successful launch and
    // is always closed.
    unsafe {
        let mut info: SHELLEXECUTEINFOW = mem::zeroed();
        info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
        info.lpVerb = verb.as_ptr();
        info.lpFile = file.as_ptr();
        info.lpParameters = params.as_ptr();
        info.nShow = SW_HIDE;

        if ShellExecuteExW(&mut info) == 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(ERROR_CANCELLED as i32) {
                bail!("administrator approval was declined");
            }
            return Err(err.into());
        }
        if info.hProcess.is_null() {
            bail!("elevated installer did not start");
        }
        WaitForSingleObject(info.hProcess, INFINITE);
        let mut code = 0u32;
        let ok = GetExitCodeProcess(info.hProcess, &mut code);
        let err = io::Error::last_os_error();
        CloseHandle(info.hProcess);
        if ok == 0 {
            return Err(err.into());
        }
        Ok(code as i32)
    }
}

/// Quotes one argument for a Windows command line (MSVC argv rules).
fn quote_arg(arg: &OsStr) -> String {
    let arg = arg.to_string_lossy();
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.into_owned();
    }
    let mut out = String::from('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => {
                backslashes += 1;
                continue;
            }
            '"' => out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1)),
            _ => out.extend(std::iter::repeat_n('\\', backslashes)),
        }
        out.push(c);
        backslashes = 0;
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_like_msvc() {
        let q = |s: &str| quote_arg(s.as_ref());
        assert_eq!(q("/S"), "/S");
        assert_eq!(q(r"C:\Temp\setup.exe"), r"C:\Temp\setup.exe");
        assert_eq!(q(r"C:\Program Files\x"), r#""C:\Program Files\x""#);
        assert_eq!(q(r"C:\dir with space\"), r#""C:\dir with space\\""#);
        assert_eq!(q(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(q(""), r#""""#);
    }
}
