//! Integration tests that exercise the real Supervisor: it spawns actual
//! processes (Windows `ping`), captures their output, and tree-kills them.

use server_supervisor_lib::ports::PortRegistry;
use server_supervisor_lib::supervisor::Supervisor;
use server_supervisor_lib::types::{ProcKind, ProcStatus};
use std::sync::Arc;
use std::time::Duration;

/// Build a Supervisor with a fresh PortRegistry rooted at the same temp dir.
fn new_sup(dir: &std::path::Path) -> Supervisor {
    Supervisor::new(dir.to_path_buf(), Arc::new(PortRegistry::new(dir.to_path_buf())))
}

/// Composite runtime id for the project/command written by `write_project`.
const ID: &str = "test:job";

fn write_project(dir: &std::path::Path, cmd: &str) {
    let root = dir.display().to_string().replace('\\', "/");
    let json = format!(
        r#"[{{"id":"test","name":"test","root":"{root}","commands":[{{"id":"job","name":"job","cmd":"{cmd}","kind":"generic","autostart":false}}]}}]"#
    );
    std::fs::write(dir.join("projects.json"), json).unwrap();
}

#[test]
fn spawn_list_logs_stop() {
    let dir = tempfile::tempdir().unwrap();
    write_project(dir.path(), "ping -n 30 127.0.0.1");
    let sup = new_sup(dir.path());

    let list = sup.list();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].status, ProcStatus::Stopped);

    sup.start(ID).unwrap();
    std::thread::sleep(Duration::from_millis(1500));

    let list = sup.list();
    assert_eq!(list[0].status, ProcStatus::Running, "should be running after start");
    assert!(list[0].pid.is_some(), "running process must have a pid");

    let logs = sup.logs(ID).unwrap();
    assert!(!logs.is_empty(), "ping output should have been captured");

    let pids = std::fs::read_to_string(dir.path().join("pids.json")).unwrap();
    assert!(pids.contains("test:job"), "pids.json should track the running process");

    sup.stop(ID).unwrap();
    let list = sup.list();
    assert_eq!(list[0].status, ProcStatus::Stopped);
    assert!(list[0].pid.is_none());
}

#[test]
fn restart_works() {
    let dir = tempfile::tempdir().unwrap();
    write_project(dir.path(), "ping -n 30 127.0.0.1");
    let sup = new_sup(dir.path());

    sup.start(ID).unwrap();
    std::thread::sleep(Duration::from_millis(800));
    assert!(sup.list()[0].pid.is_some());

    sup.restart(ID).unwrap();
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(sup.list()[0].status, ProcStatus::Running);

    sup.shutdown_all();
}

#[test]
fn unknown_id_errors() {
    let dir = tempfile::tempdir().unwrap();
    write_project(dir.path(), "ping -n 2 127.0.0.1");
    let sup = new_sup(dir.path());
    assert!(sup.start("nope").is_err());
    assert!(sup.stop("nope").is_err());
    assert!(sup.logs("nope").is_err());
}

#[test]
fn reload_rejects_generic() {
    let dir = tempfile::tempdir().unwrap();
    write_project(dir.path(), "ping -n 2 127.0.0.1");
    let sup = new_sup(dir.path());
    sup.start(ID).unwrap();
    assert!(sup.reload(ID, true).is_err());
    sup.shutdown_all();
}

#[test]
fn shutdown_kills_all_and_clears_pids() {
    let dir = tempfile::tempdir().unwrap();
    write_project(dir.path(), "ping -n 30 127.0.0.1");
    let sup = new_sup(dir.path());

    sup.start(ID).unwrap();
    std::thread::sleep(Duration::from_millis(800));
    sup.shutdown_all();

    assert_eq!(sup.list()[0].status, ProcStatus::Stopped);
    let pids = std::fs::read_to_string(dir.path().join("pids.json")).unwrap();
    assert_eq!(pids.trim(), "[]", "pids.json should be cleared on shutdown");
}

#[test]
fn config_default_written_when_missing() {
    let dir = tempfile::tempdir().unwrap();
    let sup = new_sup(dir.path());
    assert!(sup.list().is_empty());
    assert!(
        dir.path().join("projects.json").exists(),
        "a default projects.json should be created"
    );
}

#[test]
fn crud_add_remove_project_and_command() {
    let dir = tempfile::tempdir().unwrap();
    let sup = new_sup(dir.path());

    let p = sup.add_project("My App".into(), "C:/tmp".into()).unwrap();
    assert_eq!(p.id, "my-app", "id should be slugged from the name");

    let c = sup
        .add_command(&p.id, "Dev".into(), "ping -n 2 127.0.0.1".into(), ProcKind::Generic, false, false)
        .unwrap();
    let composite = format!("{}:{}", p.id, c.id);

    // Runtime map reflects the new command, and it persisted to config.
    assert!(sup.list().iter().any(|x| x.id == composite));
    let projects = sup.list_projects();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].commands.len(), 1);

    // Removing the command drops it from the runtime map and config.
    sup.remove_command(&p.id, &c.id).unwrap();
    assert!(sup.list().is_empty());

    sup.remove_project(&p.id).unwrap();
    assert!(sup.list_projects().is_empty());
}

#[test]
fn migrates_legacy_procs_json() {
    let dir = tempfile::tempdir().unwrap();
    // Old flat format with an explicit id that should be re-derived on migrate.
    std::fs::write(
        dir.path().join("procs.json"),
        r#"[{"id":"old","project":"Zng","name":"API","cmd":"ping -n 2 127.0.0.1","cwd":"C:/x","kind":"generic","autostart":false}]"#,
    )
    .unwrap();
    let sup = new_sup(dir.path());

    assert!(dir.path().join("projects.json").exists(), "migration should write projects.json");
    let projects = sup.list_projects();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].id, "zng");
    // Runtime id is now composite project/command, not the old flat "old".
    assert!(sup.list().iter().any(|x| x.id == "zng:api"));
}
