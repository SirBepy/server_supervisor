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
        .add_command(&p.id, "Dev".into(), "ping -n 2 127.0.0.1".into(), None, false, false, "".into())
        .unwrap();
    let composite = format!("{}:{}", p.id, c.id);

    // Runtime map reflects the new command, and it persisted to config.
    assert!(sup.list().iter().any(|x| x.id == composite));
    let projects = sup.list_projects();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].commands.len(), 1);

    // Removing the only command drops it from the runtime map and config, and
    // auto-removes the now-empty project.
    sup.remove_command(&p.id, &c.id).unwrap();
    assert!(sup.list().is_empty());
    assert!(sup.list_projects().is_empty(), "empty project should be auto-deleted");
}

#[test]
fn removing_last_command_deletes_project() {
    let dir = tempfile::tempdir().unwrap();
    let sup = new_sup(dir.path());

    let p = sup.add_project("My App".into(), "C:/tmp".into()).unwrap();
    let c = sup
        .add_command(&p.id, "Dev".into(), "ping -n 2 127.0.0.1".into(), None, false, false, "".into())
        .unwrap();

    // Removing the only command removes the now-empty project too.
    sup.remove_command(&p.id, &c.id).unwrap();
    assert!(
        !sup.list_projects().iter().any(|x| x.id == p.id),
        "project should be auto-deleted once its last command is gone"
    );
}

#[test]
fn removing_one_of_several_keeps_project() {
    let dir = tempfile::tempdir().unwrap();
    let sup = new_sup(dir.path());

    let p = sup.add_project("My App".into(), "C:/tmp".into()).unwrap();
    let c1 = sup
        .add_command(&p.id, "Dev".into(), "ping -n 2 127.0.0.1".into(), None, false, false, "".into())
        .unwrap();
    sup.add_command(&p.id, "Build".into(), "ping -n 3 127.0.0.1".into(), None, false, false, "".into())
        .unwrap();

    // Removing one of two commands leaves the project with the other command.
    sup.remove_command(&p.id, &c1.id).unwrap();
    let projects = sup.list_projects();
    let proj = projects.iter().find(|x| x.id == p.id);
    assert!(proj.is_some(), "project must remain while it still has a command");
    assert_eq!(proj.unwrap().commands.len(), 1, "exactly the untouched command should remain");
}

#[test]
fn adding_same_folder_twice_reuses_project() {
    let dir = tempfile::tempdir().unwrap();
    let sup = new_sup(dir.path());

    // First add: a real on-disk folder so canonicalize succeeds.
    let root = dir.path().display().to_string();
    let p1 = sup.add_project("A".into(), root.clone()).unwrap();

    // Second add: same folder, different name, trailing separator + forward
    // slashes. canonicalize must collapse these to the same path -> no dup.
    let variant = format!("{}/", root.replace('\\', "/"));
    let p2 = sup.add_project("A again".into(), variant).unwrap();

    // Exactly one project, and the second call returned the existing one
    // unchanged (same id, original name kept).
    let projects = sup.list_projects();
    assert_eq!(projects.len(), 1, "same folder must not create a duplicate project");
    assert_eq!(p2.id, p1.id, "second add should return the existing project id");
    assert_eq!(p2.name, "A", "re-entered name must be ignored; original name kept");
}

#[test]
fn add_command_infers_kind_from_cmd() {
    let dir = tempfile::tempdir().unwrap();
    let sup = new_sup(dir.path());
    let p = sup.add_project("My App".into(), "C:/tmp".into()).unwrap();

    // No kind passed (None): inferred from the command string.
    let flutter = sup
        .add_command(&p.id, "run".into(), "fvm flutter run".into(), None, false, false, "".into())
        .unwrap();
    assert_eq!(flutter.kind, ProcKind::Flutter, "flutter command -> Flutter");

    let node = sup
        .add_command(&p.id, "api".into(), "node server.js".into(), None, false, false, "".into())
        .unwrap();
    assert_eq!(node.kind, ProcKind::Generic, "non-flutter command -> Generic");

    // An explicit Some(kind) overrides inference (the /run API path).
    let forced = sup
        .add_command(&p.id, "weird".into(), "node thing.js".into(), Some(ProcKind::Flutter), false, false, "".into())
        .unwrap();
    assert_eq!(forced.kind, ProcKind::Flutter, "explicit kind overrides inference");
}

#[test]
fn adding_duplicate_command_is_noop() {
    let dir = tempfile::tempdir().unwrap();
    let sup = new_sup(dir.path());

    let p = sup.add_project("My App".into(), "C:/tmp".into()).unwrap();

    let c1 = sup
        .add_command(&p.id, "dev".into(), "npm run dev".into(), None, false, false, "".into())
        .unwrap();
    let c2 = sup
        .add_command(&p.id, "dev2".into(), "npm run dev".into(), None, false, false, "".into())
        .unwrap();

    let projects = sup.list_projects();
    assert_eq!(projects.len(), 1);
    let cmds = &projects[0].commands;
    assert_eq!(cmds.len(), 1, "duplicate cmd string must not be appended");
    assert_eq!(c2.id, c1.id, "second add should return the existing command id");

    // Runtime map has exactly one entry for this command (no double-insert).
    let composite = format!("{}:{}", p.id, c1.id);
    assert_eq!(
        sup.list().iter().filter(|x| x.id == composite).count(),
        1,
        "runtime procs map must have a single entry for the command"
    );
}

#[test]
fn update_command_edits_in_place_and_keeps_id() {
    let dir = tempfile::tempdir().unwrap();
    let sup = new_sup(dir.path());

    let p = sup.add_project("My App".into(), "C:/tmp".into()).unwrap();
    let c = sup
        .add_command(&p.id, "Dev".into(), "ping -n 2 127.0.0.1".into(), None, false, false, "".into())
        .unwrap();

    // Edit to a Flutter command: kind is inferred from the cmd string, so it
    // flips to Flutter without any kind argument.
    let updated = sup
        .update_command(
            &p.id,
            &c.id,
            "Serve".into(),
            "fvm flutter run".into(),
            true,
            true,
            "".into(),
        )
        .unwrap();

    // The id is a stable handle (keys the runtime map + logs + API path).
    assert_eq!(updated.id, c.id, "id must not change on edit");

    let projects = sup.list_projects();
    let cmd = &projects[0].commands[0];
    assert_eq!(cmd.name, "Serve");
    assert_eq!(cmd.cmd, "fvm flutter run");
    assert_eq!(cmd.kind, ProcKind::Flutter, "kind inferred from the flutter command");
    assert!(cmd.autostart);
    assert!(cmd.use_dynamic_port);

    // Runtime view reflects the edit under the same composite id.
    let info = sup
        .list()
        .into_iter()
        .find(|x| x.id == format!("{}:{}", p.id, c.id))
        .expect("command still present in runtime map");
    assert_eq!(info.name, "Serve");
    assert_eq!(info.kind, ProcKind::Flutter);
}

#[test]
fn update_command_unknown_errors() {
    let dir = tempfile::tempdir().unwrap();
    let sup = new_sup(dir.path());
    let p = sup.add_project("My App".into(), "C:/tmp".into()).unwrap();
    assert!(sup
        .update_command(&p.id, "nope", "X".into(), "ping".into(), false, false, "".into())
        .is_err());
    assert!(sup
        .update_command("nope", "job", "X".into(), "ping".into(), false, false, "".into())
        .is_err());
}

#[test]
fn update_command_rejects_edit_while_running() {
    let dir = tempfile::tempdir().unwrap();
    write_project(dir.path(), "ping -n 30 127.0.0.1");
    let sup = new_sup(dir.path());

    sup.start(ID).unwrap();
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(sup.list()[0].status, ProcStatus::Running, "must be running before edit");

    // A running command is locked: editing it is rejected, never silently
    // relaunched. The UI hides the edit button while running for the same reason.
    let err = sup
        .update_command(
            "test",
            "job",
            "job".into(),
            "ping -n 31 127.0.0.1".into(),
            false,
            false,
            "".into(),
        )
        .unwrap_err();
    assert!(err.contains("stop the command"), "running edit should be rejected: {err}");

    // After stopping, the same edit applies in place under the stable id.
    sup.stop(ID).unwrap();
    let updated = sup
        .update_command(
            "test",
            "job",
            "job".into(),
            "ping -n 31 127.0.0.1".into(),
            false,
            false,
            "".into(),
        )
        .unwrap();
    assert_eq!(updated.cmd, "ping -n 31 127.0.0.1");
    assert_eq!(sup.list_projects()[0].commands[0].cmd, "ping -n 31 127.0.0.1");

    sup.shutdown_all();
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

#[test]
fn ensure_and_run_registers_starts_and_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("server.js"),
        "const p=process.env.PORT;require('http').createServer((_,r)=>r.end('ok')).listen(p,()=>console.log('LISTENING '+p));",
    )
    .unwrap();
    let sup = new_sup(dir.path());
    let root = dir.path().to_str().unwrap();

    let info = sup
        .ensure_and_run(root, "node server.js", None, None, true, "".into())
        .unwrap();
    assert_eq!(info.status, ProcStatus::Running);
    let port = info.port.expect("dynamic port should be assigned");
    assert!((42000..49000).contains(&port));

    // Idempotent: same root+cmd reuses the same project/command (no duplicate).
    let info2 = sup
        .ensure_and_run(root, "node server.js", None, None, true, "".into())
        .unwrap();
    assert_eq!(info2.id, info.id);
    assert_eq!(sup.list().len(), 1, "no duplicate registration");

    sup.stop(&info.id).unwrap();
}
