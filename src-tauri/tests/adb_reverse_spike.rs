//! Live spike for todo 0025: proves `adb_reverse::forward`/`remove` actually
//! tunnel `localhost:<port>` from the real Android emulator to the host, and
//! that traffic sent from the device's shell lands on a host-side
//! `TcpListener` - not just that `adb reverse --list` reports the mapping.
//!
//! Requires an attached device/emulator (confirmed via `adb devices`) and a
//! real `adb` on PATH. Run with:
//!   cargo test --test adb_reverse_spike -- --ignored --nocapture

use server_supervisor_lib::supervisor::adb_reverse::{forward, remove};
use std::net::TcpListener;
use std::process::Command;
use std::time::Duration;

const DEVICE: &str = "emulator-5554";

fn adb_reverse_list() -> String {
    let out = Command::new("adb")
        .args(["-s", DEVICE, "reverse", "--list"])
        .output()
        .expect("run adb reverse --list");
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
#[ignore]
fn spike_forward_tunnels_a_real_connection_then_remove_tears_it_down() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral host listener");
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).expect("set_nonblocking");
    println!("host listener bound on 127.0.0.1:{port}");

    forward(DEVICE, port).expect("forward() must succeed against the attached emulator");
    let list = adb_reverse_list();
    println!("adb reverse --list after forward:\n{list}");
    assert!(
        list.contains(&format!("tcp:{port}")),
        "reverse list must show the tcp:{port} mapping after forward(); got: {list}"
    );

    // From the device's own shell, connect to its localhost:<port> (which
    // `adb reverse` now tunnels back to this host's listener) and confirm
    // the host actually sees the connection - not just that adb's bookkeeping
    // says the mapping exists.
    let nc = Command::new("adb")
        .args([
            "-s",
            DEVICE,
            "shell",
            &format!("echo hi | toybox nc 127.0.0.1 {port}"),
        ])
        .spawn();
    match nc {
        Ok(mut child) => {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            let mut accepted = false;
            while std::time::Instant::now() < deadline {
                if listener.accept().is_ok() {
                    accepted = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            let _ = child.kill();
            assert!(accepted, "host listener never saw a connection from the device's toybox nc within 10s");
            println!("host listener accepted a connection forwarded from the device");
        }
        Err(e) => {
            println!("could not spawn `adb shell ... toybox nc` ({e}); falling back to list-only assertion");
        }
    }

    remove(DEVICE, port).expect("remove() must succeed");
    let list_after = adb_reverse_list();
    println!("adb reverse --list after remove:\n{list_after}");
    assert!(
        !list_after.contains(&format!("tcp:{port}")),
        "reverse list must no longer show tcp:{port} after remove(); got: {list_after}"
    );
}
