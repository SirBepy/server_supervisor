//! Integration tests that exercise the real Supervisor: it spawns actual
//! processes (Windows `ping`), captures their output, and tree-kills them.

use server_supervisor_lib::supervisor::Supervisor;
use server_supervisor_lib::types::ProcStatus;
use std::time::Duration;

fn write_procs(dir: &std::path::Path, cmd: &str) {
    let cwd = dir.display().to_string().replace('\\', "/");
    let json = format!(
        r#"[{{"id":"t1","project":"test","name":"job","cmd":"{cmd}","cwd":"{cwd}","kind":"generic","autostart":false}}]"#
    );
    std::fs::write(dir.join("procs.json"), json).unwrap();
}

#[test]
fn spawn_list_logs_stop() {
    let dir = tempfile::tempdir().unwrap();
    write_procs(dir.path(), "ping -n 30 127.0.0.1");
    let sup = Supervisor::new(dir.path().to_path_buf());

    let list = sup.list();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].status, ProcStatus::Stopped);

    sup.start("t1").unwrap();
    std::thread::sleep(Duration::from_millis(1500));

    let list = sup.list();
    assert_eq!(list[0].status, ProcStatus::Running, "should be running after start");
    assert!(list[0].pid.is_some(), "running process must have a pid");

    let logs = sup.logs("t1").unwrap();
    assert!(!logs.is_empty(), "ping output should have been captured");

    let pids = std::fs::read_to_string(dir.path().join("pids.json")).unwrap();
    assert!(pids.contains("t1"), "pids.json should track the running process");

    sup.stop("t1").unwrap();
    let list = sup.list();
    assert_eq!(list[0].status, ProcStatus::Stopped);
    assert!(list[0].pid.is_none());
}

#[test]
fn restart_works() {
    let dir = tempfile::tempdir().unwrap();
    write_procs(dir.path(), "ping -n 30 127.0.0.1");
    let sup = Supervisor::new(dir.path().to_path_buf());

    sup.start("t1").unwrap();
    std::thread::sleep(Duration::from_millis(800));
    let first_pid = sup.list()[0].pid;
    assert!(first_pid.is_some());

    sup.restart("t1").unwrap();
    std::thread::sleep(Duration::from_millis(800));
    let list = sup.list();
    assert_eq!(list[0].status, ProcStatus::Running);

    sup.shutdown_all();
}

#[test]
fn unknown_id_errors() {
    let dir = tempfile::tempdir().unwrap();
    write_procs(dir.path(), "ping -n 2 127.0.0.1");
    let sup = Supervisor::new(dir.path().to_path_buf());
    assert!(sup.start("nope").is_err());
    assert!(sup.stop("nope").is_err());
    assert!(sup.logs("nope").is_err());
}

#[test]
fn reload_rejects_generic() {
    let dir = tempfile::tempdir().unwrap();
    write_procs(dir.path(), "ping -n 2 127.0.0.1");
    let sup = Supervisor::new(dir.path().to_path_buf());
    sup.start("t1").unwrap();
    // reload is flutter-only; a generic process must reject it.
    assert!(sup.reload("t1", true).is_err());
    sup.shutdown_all();
}

#[test]
fn shutdown_kills_all_and_clears_pids() {
    let dir = tempfile::tempdir().unwrap();
    write_procs(dir.path(), "ping -n 30 127.0.0.1");
    let sup = Supervisor::new(dir.path().to_path_buf());

    sup.start("t1").unwrap();
    std::thread::sleep(Duration::from_millis(800));
    sup.shutdown_all();

    let list = sup.list();
    assert_eq!(list[0].status, ProcStatus::Stopped);
    let pids = std::fs::read_to_string(dir.path().join("pids.json")).unwrap();
    assert_eq!(pids.trim(), "[]", "pids.json should be cleared on shutdown");
}

#[test]
fn config_default_written_when_missing() {
    let dir = tempfile::tempdir().unwrap();
    let sup = Supervisor::new(dir.path().to_path_buf());
    assert!(sup.list().is_empty());
    assert!(
        dir.path().join("procs.json").exists(),
        "a default procs.json should be created"
    );
}
