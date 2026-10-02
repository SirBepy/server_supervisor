//! Shared between `audio_spike.rs` and `window_spike.rs` (and their own
//! `audio_common`/`window_host`/`window_capture` helper modules): process
//! tree enumeration and force-kill, the only code both media_spike halves
//! needed. Split out of the former `tests/media_spike.rs` (todo 0060).
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::os::windows::process::CommandExt;
use std::process::Command;

use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};

pub(crate) const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub(crate) fn process_tree(root: u32) -> HashSet<u32> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).expect("toolhelp snapshot");
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        if Process32FirstW(snap, &mut e).is_ok() {
            loop {
                children.entry(e.th32ParentProcessID).or_default().push(e.th32ProcessID);
                if Process32NextW(snap, &mut e).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    let mut out = HashSet::from([root]);
    let mut stack = vec![root];
    while let Some(p) = stack.pop() {
        for &c in children.get(&p).into_iter().flatten() {
            if c != p && out.insert(c) {
                stack.push(c);
            }
        }
    }
    out
}

pub(crate) fn kill_tree(pid: u32) {
    let _ = Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
}
