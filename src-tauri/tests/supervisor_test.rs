//! Integration tests that exercise the real Supervisor: it spawns actual
//! processes (Windows `ping`), captures their output, and tree-kills them.

use server_supervisor_lib::ports::PortRegistry;
use server_supervisor_lib::supervisor::Supervisor;
use server_supervisor_lib::types::{CommandParam, ParamValue, ProcKind, ProcStatus, Role};
use std::sync::Arc;
use std::time::Duration;

fn device_param(last_value: &str) -> CommandParam {
    CommandParam {
        name: "device".to_string(),
        label: "Device".to_string(),
        values: vec![
            ParamValue { value: "web-server".to_string(), label: "Web Server".to_string(), flag: "-d web-server".to_string() },
            ParamValue { value: "chrome".to_string(), label: "Chrome".to_string(), flag: "-d chrome".to_string() },
        ],
        last_value: Some(last_value.to_string()),
    }
}

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
fn sample_tick_fills_subtree_memory_read_by_list() {
    let dir = tempfile::tempdir().unwrap();
    write_project(dir.path(), "ping -n 30 127.0.0.1");
    let sup = new_sup(dir.path());

    // Stopped: no live-looking RAM figure.
    assert_eq!(sup.list()[0].mem_bytes, None, "stopped process reports no RAM");

    sup.start(ID).unwrap();
    std::thread::sleep(Duration::from_millis(1500));

    // RAM is now sampled off the UI thread by the background sampler, NOT inline
    // in list(). Before a tick, list() reports nothing.
    assert_eq!(
        sup.list()[0].mem_bytes,
        None,
        "list() must not sample RAM itself (that was the UI-thread lag)"
    );

    // After a sampler tick, sysinfo has summed the spawned `cmd`/`ping` subtree
    // and cached it, so list() reports a present, nonzero figure (proves the
    // end-to-end wiring, not just the pure tree-walk unit test in supervisor::mem).
    sup.sample_tick();
    let mem = sup.list()[0].mem_bytes;
    assert!(matches!(mem, Some(b) if b > 0), "running process must report nonzero RAM, got {mem:?}");

    sup.stop(ID).unwrap();
    // stop() clears the cache immediately, so list() reflects it without waiting
    // for the next sampler tick.
    assert_eq!(sup.list()[0].mem_bytes, None, "stopped again reports no RAM");
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
        .add_command(&p.id, "Dev".into(), "ping -n 2 127.0.0.1".into(), None, false, false, None, "".into(), None, false, Vec::new())
        .unwrap()
        .command;
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
fn crud_rename_project() {
    let dir = tempfile::tempdir().unwrap();
    let sup = new_sup(dir.path());

    let p = sup.add_project("My App".into(), "C:/tmp".into()).unwrap();

    // Rename mutates only the display name; the id is a stable handle and must
    // not change (it keys the runtime map, logs, and API paths).
    let renamed = sup.rename_project(&p.id, "New Name".into()).unwrap();
    assert_eq!(renamed.name, "New Name", "name should be the trimmed new value");
    assert_eq!(renamed.id, p.id, "id must not change on rename");

    // Persistence: a fresh Supervisor over the SAME data dir reloads the new
    // name from disk (proves config::save wrote it, not just an in-memory edit).
    let reloaded = new_sup(dir.path());
    let projects = reloaded.list_projects();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].id, p.id);
    assert_eq!(projects[0].name, "New Name", "renamed name should survive a reload");

    // Empty-name guard: a whitespace-only name trims to empty and is rejected.
    assert!(
        sup.rename_project(&p.id, "   ".into()).is_err(),
        "blank name must be rejected"
    );

    // Unknown-id guard.
    assert!(
        sup.rename_project("nope", "X".into()).is_err(),
        "unknown project id must be rejected"
    );
}

#[test]
fn removing_last_command_deletes_project() {
    let dir = tempfile::tempdir().unwrap();
    let sup = new_sup(dir.path());

    let p = sup.add_project("My App".into(), "C:/tmp".into()).unwrap();
    let c = sup
        .add_command(&p.id, "Dev".into(), "ping -n 2 127.0.0.1".into(), None, false, false, None, "".into(), None, false, Vec::new())
        .unwrap()
        .command;

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
        .add_command(&p.id, "Dev".into(), "ping -n 2 127.0.0.1".into(), None, false, false, None, "".into(), None, false, Vec::new())
        .unwrap()
        .command;
    sup.add_command(&p.id, "Build".into(), "ping -n 3 127.0.0.1".into(), None, false, false, None, "".into(), None, false, Vec::new())
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
        .add_command(&p.id, "run".into(), "fvm flutter run".into(), None, false, false, None, "".into(), None, false, Vec::new())
        .unwrap()
        .command;
    assert_eq!(flutter.kind, ProcKind::Flutter, "flutter command -> Flutter");

    let node = sup
        .add_command(&p.id, "api".into(), "node server.js".into(), None, false, false, None, "".into(), None, false, Vec::new())
        .unwrap()
        .command;
    assert_eq!(node.kind, ProcKind::Generic, "non-flutter command -> Generic");

    // An explicit Some(kind) overrides inference (the /run API path).
    let forced = sup
        .add_command(&p.id, "weird".into(), "node thing.js".into(), Some(ProcKind::Flutter), false, false, None, "".into(), None, false, Vec::new())
        .unwrap()
        .command;
    assert_eq!(forced.kind, ProcKind::Flutter, "explicit kind overrides inference");
}

#[test]
fn adding_duplicate_command_is_noop() {
    let dir = tempfile::tempdir().unwrap();
    let sup = new_sup(dir.path());

    let p = sup.add_project("My App".into(), "C:/tmp".into()).unwrap();

    let c1 = sup
        .add_command(&p.id, "dev".into(), "npm run dev".into(), None, false, false, None, "".into(), None, false, Vec::new())
        .unwrap()
        .command;
    let c2 = sup
        .add_command(&p.id, "dev2".into(), "npm run dev".into(), None, false, false, None, "".into(), None, false, Vec::new())
        .unwrap()
        .command;

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
        .add_command(&p.id, "Dev".into(), "ping -n 2 127.0.0.1".into(), None, false, false, None, "".into(), None, false, Vec::new())
        .unwrap()
        .command;

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
            None,
            "".into(),
            None,
            false,
            None,
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
        .update_command(&p.id, "nope", "X".into(), "ping".into(), false, false, None, "".into(), None, false, None)
        .is_err());
    assert!(sup
        .update_command("nope", "job", "X".into(), "ping".into(), false, false, None, "".into(), None, false, None)
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
            None,
            "".into(),
            None,
            false,
            None,
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
            None,
            "".into(),
            None,
            false,
            None,
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
        .ensure_and_run(root, "node server.js", None, None, true, None, "".into(), None)
        .unwrap()
        .info;
    assert_eq!(info.status, ProcStatus::Running);
    let port = info.port.expect("dynamic port should be assigned");
    assert!((42000..49000).contains(&port));

    // Idempotent: same root+cmd reuses the same project/command (no duplicate).
    let info2 = sup
        .ensure_and_run(root, "node server.js", None, None, true, None, "".into(), None)
        .unwrap()
        .info;
    assert_eq!(info2.id, info.id);
    assert_eq!(sup.list().len(), 1, "no duplicate registration");

    sup.stop(&info.id).unwrap();
}

#[test]
fn render_and_compare_matches_existing_template_and_sets_last_value() {
    let dir = tempfile::tempdir().unwrap();
    let sup = new_sup(dir.path());
    let p = sup.add_project("My App".into(), "C:/tmp".into()).unwrap();

    let created = sup
        .add_command(
            &p.id, "run".into(), "echo {DEVICE}".into(), None, false, false, None, "".into(), None, false,
            vec![device_param("web-server")],
        )
        .unwrap()
        .command;

    // Posting the CONCRETE rendered cmd (no template, no `params` object)
    // must render-and-compare match the existing templated command, not
    // fork a new one.
    let outcome = sup
        .add_command(&p.id, "whatever".into(), "echo -d chrome".into(), None, false, false, None, "".into(), None, false, Vec::new())
        .unwrap();
    assert_eq!(outcome.command.id, created.id, "must return the existing templated command, not a fork");
    assert!(outcome.param_mismatch.is_none(), "a stopped match is never a mismatch");

    let projects = sup.list_projects();
    assert_eq!(projects[0].commands.len(), 1, "command count must be unchanged (no fork)");
    assert_eq!(
        projects[0].commands[0].params[0].last_value.as_deref(),
        Some("chrome"),
        "the matched combination must be written back onto last_value"
    );
}

#[test]
fn render_and_compare_miss_still_forks_a_new_command() {
    let dir = tempfile::tempdir().unwrap();
    let sup = new_sup(dir.path());
    let p = sup.add_project("My App".into(), "C:/tmp".into()).unwrap();

    let mode = CommandParam {
        name: "mode".to_string(),
        label: "Mode".to_string(),
        values: vec![ParamValue { value: "dev".to_string(), label: "Dev".to_string(), flag: "dev".to_string() }],
        last_value: Some("dev".to_string()),
    };
    sup.add_command(&p.id, "run".into(), "npm run {MODE}".into(), None, false, false, None, "".into(), None, false, vec![mode])
        .unwrap();

    // "npm run preview" matches NO combination of the templated command's
    // values - must fork, exactly like today's `dev`-vs-`preview` case.
    sup.add_command(&p.id, "preview".into(), "npm run preview".into(), None, false, false, None, "".into(), None, false, Vec::new())
        .unwrap();

    let projects = sup.list_projects();
    assert_eq!(projects[0].commands.len(), 2, "an unrelated cmd must fork, never merge into the templated command");
}

#[test]
fn render_and_compare_respects_the_64_combo_cap() {
    let dir = tempfile::tempdir().unwrap();
    let sup = new_sup(dir.path());
    let p = sup.add_project("My App".into(), "C:/tmp".into()).unwrap();

    fn axis(name: &str) -> CommandParam {
        CommandParam {
            name: name.to_string(),
            label: name.to_string(),
            values: (0..5)
                .map(|i| {
                    let (id, flag) = if i == 4 {
                        (format!("{name}-marker"), format!("--{name}=MARKER"))
                    } else {
                        (format!("{name}-{i}"), format!("--{name}={i}"))
                    };
                    ParamValue { value: id, label: flag.clone(), flag }
                })
                .collect(),
            last_value: None,
        }
    }
    // 3 params x 5 values = 125 combinations; only the one where every axis
    // picks its LAST value renders to the marker cmd below. That combo's
    // index in ANY consistent enumeration order is the single highest one
    // (124 of 0..124), always past the 64-render cap - so if the cap is
    // enforced, it must never be found.
    let params = vec![axis("a"), axis("b"), axis("c")];
    sup.add_command(&p.id, "run".into(), "echo {A} {B} {C}".into(), None, false, false, None, "".into(), None, false, params)
        .unwrap();

    let marker_cmd = "echo --a=MARKER --b=MARKER --c=MARKER";
    let outcome = sup
        .add_command(&p.id, "marker".into(), marker_cmd.into(), None, false, false, None, "".into(), None, false, Vec::new())
        .unwrap();

    let projects = sup.list_projects();
    assert_eq!(projects[0].commands.len(), 2, "the beyond-cap combo must fork a new Command, not match");
    assert_eq!(outcome.command.cmd, marker_cmd, "the forked command carries the posted cmd verbatim");
}

#[test]
fn render_and_compare_stopped_hit_refreshes_the_live_spec() {
    let dir = tempfile::tempdir().unwrap();
    let sup = new_sup(dir.path());
    let p = sup.add_project("My App".into(), "C:/tmp".into()).unwrap();

    let created = sup
        .add_command(
            &p.id, "run".into(), "echo {DEVICE}".into(), None, false, false, None, "".into(), None, false,
            vec![device_param("web-server")],
        )
        .unwrap()
        .command;

    sup.add_command(&p.id, "whatever".into(), "echo -d chrome".into(), None, false, false, None, "".into(), None, false, Vec::new())
        .unwrap();

    // The test that fails if only the persisted `Command` was updated: the
    // LIVE `ManagedProc.spec` must also carry the matched combination, since
    // `start`/`spawn.rs` read only `self.spec`, never re-derive it from
    // `Command`. `resolved_cmd` is computed straight off that live spec.
    let id = format!("{}:{}", p.id, created.id);
    let info = sup.list().into_iter().find(|x| x.id == id).unwrap();
    assert_eq!(
        info.resolved_cmd.as_deref(),
        Some("echo -d chrome"),
        "the live spec must resolve to the matched combination, not the stale previous one"
    );
}

#[test]
fn render_and_compare_running_mismatch_leaves_running_proc_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let sup = new_sup(dir.path());
    let p = sup.add_project("My App".into(), "C:/tmp".into()).unwrap();

    // A cheap long-running command (mirrors `long_running_command` in
    // `registry.rs`'s own tests), not flutter - the param text is spliced
    // into a harmless `set` no-op so an unrecognized flag can never make
    // `ping` itself exit early.
    let created = sup
        .add_command(
            &p.id,
            "run".into(),
            "set CHOICE={DEVICE} & ping -n 30 127.0.0.1".into(),
            None, false, false, None, "".into(), None, false,
            vec![device_param("web-server")],
        )
        .unwrap()
        .command;
    let id = format!("{}:{}", p.id, created.id);
    sup.start(&id).unwrap();
    std::thread::sleep(Duration::from_millis(800));
    let pid_before = sup.list().into_iter().find(|x| x.id == id).unwrap().pid;
    assert!(pid_before.is_some(), "must be running before the mismatch post");

    // Posting the CHROME combo while WEB-SERVER is running:
    // nothing written, nothing restarted, reported as a mismatch.
    let outcome = sup
        .add_command(
            &p.id,
            "whatever".into(),
            "set CHOICE=-d chrome & ping -n 30 127.0.0.1".into(),
            None, false, false, None, "".into(), None, false, Vec::new(),
        )
        .unwrap();
    assert_eq!(outcome.command.id, created.id);
    let mismatch = outcome.param_mismatch.expect("must report a mismatch");
    assert_eq!(mismatch.running.get("device").map(String::as_str), Some("web-server"));
    assert_eq!(mismatch.requested.get("device").map(String::as_str), Some("chrome"));

    let after = sup.list().into_iter().find(|x| x.id == id).unwrap();
    assert_eq!(after.pid, pid_before, "the running process must be untouched (same pid)");
    let projects = sup.list_projects();
    assert_eq!(
        projects[0].commands[0].params[0].last_value.as_deref(),
        Some("web-server"),
        "last_value must stay untouched on a mismatch"
    );

    sup.stop(&id).unwrap();
}

#[test]
fn set_command_param_restarts_running_and_skips_on_unchanged_value() {
    let dir = tempfile::tempdir().unwrap();
    let sup = new_sup(dir.path());
    let p = sup.add_project("My App".into(), "C:/tmp".into()).unwrap();

    let created = sup
        .add_command(
            &p.id,
            "run".into(),
            "set CHOICE={DEVICE} & ping -n 30 127.0.0.1".into(),
            None, false, false, None, "".into(), None, false,
            vec![device_param("web-server")],
        )
        .unwrap()
        .command;
    let id = format!("{}:{}", p.id, created.id);
    sup.start(&id).unwrap();
    std::thread::sleep(Duration::from_millis(800));
    let pid_before = sup.list().into_iter().find(|x| x.id == id).unwrap().pid.expect("running");

    // Unchanged value: must not bounce the live process.
    sup.set_command_param(&p.id, &created.id, "device", "web-server").unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let pid_unchanged = sup.list().into_iter().find(|x| x.id == id).unwrap().pid;
    assert_eq!(pid_unchanged, Some(pid_before), "a same-value re-pick must not bounce the process");

    // Changed value on a RUNNING command: restarts into the new variant.
    sup.set_command_param(&p.id, &created.id, "device", "chrome").unwrap();
    std::thread::sleep(Duration::from_millis(800));
    let after = sup.list().into_iter().find(|x| x.id == id).unwrap();
    assert_ne!(after.pid, Some(pid_before), "a changed value on a running command must restart it");
    assert_eq!(after.resolved_cmd.as_deref(), Some("set CHOICE=-d chrome & ping -n 30 127.0.0.1"));

    sup.stop(&id).unwrap();
}

#[test]
fn update_command_params_none_keeps_some_empty_clears() {
    let dir = tempfile::tempdir().unwrap();
    let sup = new_sup(dir.path());
    let p = sup.add_project("My App".into(), "C:/tmp".into()).unwrap();

    let created = sup
        .add_command(
            &p.id, "run".into(), "echo {DEVICE}".into(), None, false, false, None, "".into(), None, false,
            vec![device_param("chrome")],
        )
        .unwrap()
        .command;

    // Omitted (`None`): a full-replace edit keeps the existing params.
    let kept = sup
        .update_command(&p.id, &created.id, "run".into(), "echo {DEVICE}".into(), false, false, None, "".into(), None, false, None)
        .unwrap();
    assert_eq!(kept.params.len(), 1, "None must keep the existing params");

    // Explicit empty vec: clears them.
    let cleared = sup
        .update_command(
            &p.id, &created.id, "run".into(), "echo {DEVICE}".into(), false, false, None, "".into(), None, false, Some(Vec::new()),
        )
        .unwrap();
    assert!(cleared.params.is_empty(), "Some(vec![]) must clear params");
}

#[test]
fn add_command_authoring_validation_rejects_bad_param_shapes_and_persists_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let sup = new_sup(dir.path());
    let p = sup.add_project("My App".into(), "C:/tmp".into()).unwrap();

    let reserved = vec![CommandParam {
        name: "port".to_string(),
        label: "Port".to_string(),
        values: vec![ParamValue { value: "a".to_string(), label: "a".to_string(), flag: "".to_string() }],
        last_value: None,
    }];
    assert!(sup
        .add_command(&p.id, "x".into(), "echo {PORT}".into(), None, false, false, None, "".into(), None, false, reserved)
        .is_err());

    let dup = vec![
        CommandParam {
            name: "device".to_string(),
            label: "d".to_string(),
            values: vec![ParamValue { value: "a".to_string(), label: "a".to_string(), flag: "".to_string() }],
            last_value: None,
        },
        CommandParam {
            name: "DEVICE".to_string(),
            label: "d2".to_string(),
            values: vec![ParamValue { value: "b".to_string(), label: "b".to_string(), flag: "".to_string() }],
            last_value: None,
        },
    ];
    assert!(sup
        .add_command(&p.id, "y".into(), "echo {DEVICE}".into(), None, false, false, None, "".into(), None, false, dup)
        .is_err());

    assert!(
        sup.list_projects()[0].commands.is_empty(),
        "neither rejected authoring call must persist a command"
    );
}

/// Two-value device param matching `src1`'s/`src2`'s rendered flags, in
/// source order (value 0 belongs to the first source).
fn device_values_param() -> CommandParam {
    CommandParam {
        name: "device".to_string(),
        label: "Device".to_string(),
        values: vec![
            ParamValue { value: "web-server".to_string(), label: "Web Server".to_string(), flag: "-d web-server".to_string() },
            ParamValue { value: "chrome".to_string(), label: "Chrome".to_string(), flag: "-d chrome".to_string() },
        ],
        last_value: None,
    }
}

#[test]
fn combine_commands_merges_two_sources_into_one_param_command() {
    let dir = tempfile::tempdir().unwrap();
    let sup = new_sup(dir.path());
    let p = sup.add_project("My App".into(), "C:/tmp".into()).unwrap();

    let src1 = sup
        .add_command(&p.id, "web-server".into(), "echo -d web-server".into(), None, false, false, None, "".into(), None, false, Vec::new())
        .unwrap()
        .command;
    let src2 = sup
        .add_command(&p.id, "chrome".into(), "echo -d chrome".into(), None, false, false, None, "".into(), None, false, Vec::new())
        .unwrap()
        .command;

    let combined = sup
        .combine_commands(&p.id, vec![src1.id.clone(), src2.id.clone()], "Run".into(), "echo {DEVICE}".into(), device_values_param())
        .unwrap();

    assert_eq!(combined.cmd, "echo {DEVICE}");
    assert_eq!(combined.params.len(), 1, "exactly one param axis");
    assert_eq!(combined.params[0].values.len(), 2, "one value per source");
    assert_eq!(
        combined.params[0].last_value.as_deref(),
        Some("web-server"),
        "last_value defaults to the FIRST source's value id"
    );

    let projects = sup.list_projects();
    assert_eq!(projects[0].commands.len(), 1, "both sources must be gone, replaced by one command");
    assert!(
        !projects[0].commands.iter().any(|c| c.id == src1.id || c.id == src2.id),
        "neither source id survives"
    );

    // The live spec resolves each value back to the exact source cmd it came
    // from - the substitution round-trip the migration promises.
    let id = format!("{}:{}", p.id, combined.id);
    let info = sup.list().into_iter().find(|x| x.id == id).unwrap();
    assert_eq!(info.resolved_cmd.as_deref(), Some("echo -d web-server"), "defaults to the first source's rendering");

    sup.set_command_param(&p.id, &combined.id, "device", "chrome").unwrap();
    let info2 = sup.list().into_iter().find(|x| x.id == id).unwrap();
    assert_eq!(info2.resolved_cmd.as_deref(), Some("echo -d chrome"), "switching the value reproduces the other source's cmd");

    // Persisted to disk, not just in-memory.
    let reloaded = new_sup(dir.path());
    let rp = reloaded.list_projects();
    assert_eq!(rp[0].commands.len(), 1);
    assert_eq!(rp[0].commands[0].cmd, "echo {DEVICE}");
    assert_eq!(rp[0].commands[0].params[0].values.len(), 2);
}

#[test]
fn combine_commands_copies_non_cmd_fields_from_the_first_source() {
    let dir = tempfile::tempdir().unwrap();
    let sup = new_sup(dir.path());
    let p = sup.add_project("My App".into(), "C:/tmp".into()).unwrap();

    let src1 = sup
        .add_command(
            &p.id, "web-server".into(), "echo -d web-server".into(), None, true, false, None, "FOO=bar".into(),
            Some(Role::Frontend), true, Vec::new(),
        )
        .unwrap()
        .command;
    sup.set_command_sound(&p.id, &src1.id, true).unwrap();
    sup.set_command_headless(&p.id, &src1.id, true).unwrap();
    let src2 = sup
        .add_command(&p.id, "chrome".into(), "echo -d chrome".into(), None, false, false, None, "".into(), None, false, Vec::new())
        .unwrap()
        .command;

    let combined = sup
        .combine_commands(&p.id, vec![src1.id.clone(), src2.id.clone()], "Run".into(), "echo {DEVICE}".into(), device_values_param())
        .unwrap();

    assert!(combined.autostart, "autostart copied from the first source");
    assert_eq!(combined.env, "FOO=bar", "env copied from the first source");
    assert_eq!(combined.role, Some(Role::Frontend), "role copied from the first source");
    assert!(combined.dock_window, "dock_window copied from the first source");
    assert!(combined.play_sound, "play_sound copied from the first source");
    assert!(combined.dock_headless, "dock_headless copied from the first source");
}

#[test]
fn combine_commands_refuses_invalid_inputs_and_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let sup = new_sup(dir.path());
    let p = sup.add_project("My App".into(), "C:/tmp".into()).unwrap();
    let other = sup.add_project("Other".into(), "C:/tmp2".into()).unwrap();

    let src1 = sup
        .add_command(&p.id, "web-server".into(), "echo -d web-server".into(), None, false, false, None, "".into(), None, false, Vec::new())
        .unwrap()
        .command;
    let src2 = sup
        .add_command(&p.id, "chrome".into(), "echo -d chrome".into(), None, false, false, None, "".into(), None, false, Vec::new())
        .unwrap()
        .command;
    let other_cmd = sup
        .add_command(&other.id, "job".into(), "echo job".into(), None, false, false, None, "".into(), None, false, Vec::new())
        .unwrap()
        .command;

    // Fewer than 2 source ids.
    assert!(sup
        .combine_commands(&p.id, vec![src1.id.clone()], "Run".into(), "echo {DEVICE}".into(), device_values_param())
        .is_err());

    // Duplicate source id.
    assert!(sup
        .combine_commands(&p.id, vec![src1.id.clone(), src1.id.clone()], "Run".into(), "echo {DEVICE}".into(), device_values_param())
        .is_err());

    // Unknown id.
    assert!(sup
        .combine_commands(&p.id, vec![src1.id.clone(), "nope".into()], "Run".into(), "echo {DEVICE}".into(), device_values_param())
        .is_err());

    // An id from ANOTHER project is just as unknown here - never merges across projects.
    assert!(sup
        .combine_commands(&p.id, vec![src1.id.clone(), other_cmd.id.clone()], "Run".into(), "echo {DEVICE}".into(), device_values_param())
        .is_err());

    // Template missing the param's {NAME} token.
    assert!(sup
        .combine_commands(&p.id, vec![src1.id.clone(), src2.id.clone()], "Run".into(), "echo plain".into(), device_values_param())
        .is_err());

    // values.len() != source_ids.len().
    let mut three_values = device_values_param();
    three_values.values.push(ParamValue { value: "android".to_string(), label: "Android".to_string(), flag: "-d android".to_string() });
    assert!(sup
        .combine_commands(&p.id, vec![src1.id.clone(), src2.id.clone()], "Run".into(), "echo {DEVICE}".into(), three_values)
        .is_err());

    // Nothing above ever wrote anything: both projects are exactly as they started.
    let projects = sup.list_projects();
    let mine = projects.iter().find(|x| x.id == p.id).unwrap();
    assert_eq!(mine.commands.len(), 2, "no refused call may remove or merge a source");
    let theirs = projects.iter().find(|x| x.id == other.id).unwrap();
    assert_eq!(theirs.commands.len(), 1);
}

#[test]
fn combine_commands_refuses_a_running_source_and_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let sup = new_sup(dir.path());
    let p = sup.add_project("My App".into(), "C:/tmp".into()).unwrap();

    let src1 = sup
        .add_command(&p.id, "web-server".into(), "ping -n 30 127.0.0.1".into(), None, false, false, None, "".into(), None, false, Vec::new())
        .unwrap()
        .command;
    let src2 = sup
        .add_command(&p.id, "chrome".into(), "echo -d chrome".into(), None, false, false, None, "".into(), None, false, Vec::new())
        .unwrap()
        .command;
    let id1 = format!("{}:{}", p.id, src1.id);
    sup.start(&id1).unwrap();
    std::thread::sleep(Duration::from_millis(800));
    let pid_before = sup.list().into_iter().find(|x| x.id == id1).unwrap().pid;
    assert!(pid_before.is_some(), "must be running before the combine attempt");

    let err = sup
        .combine_commands(&p.id, vec![src1.id.clone(), src2.id.clone()], "Run".into(), "{DEVICE}".into(), device_values_param())
        .unwrap_err();
    assert!(err.contains("web-server"), "error must name the running source: {err}");

    assert_eq!(sup.list_projects()[0].commands.len(), 2, "a refused combine must not touch either source");
    let after = sup.list().into_iter().find(|x| x.id == id1).unwrap();
    assert_eq!(after.pid, pid_before, "the running source must be untouched");

    sup.stop(&id1).unwrap();
}

#[test]
fn combine_commands_transfers_the_first_sources_fixed_port() {
    let dir = tempfile::tempdir().unwrap();
    let ports = Arc::new(PortRegistry::new(dir.path().to_path_buf()));
    let sup = Supervisor::new(dir.path().to_path_buf(), ports.clone());
    let p = sup.add_project("My App".into(), "C:/tmp".into()).unwrap();

    let src1 = sup
        .add_command(&p.id, "web-server".into(), "echo -d web-server".into(), None, false, true, Some(45001), "".into(), None, false, Vec::new())
        .unwrap()
        .command;
    let src2 = sup
        .add_command(&p.id, "chrome".into(), "echo -d chrome".into(), None, false, false, None, "".into(), None, false, Vec::new())
        .unwrap()
        .command;

    let owner1 = format!("{}:{}", p.id, src1.id);
    assert!(ports.list().iter().any(|e| e.owner == owner1 && e.port == 45001), "fixed port must be reserved before combining");

    let combined = sup
        .combine_commands(&p.id, vec![src1.id.clone(), src2.id.clone()], "Run".into(), "echo {DEVICE}".into(), device_values_param())
        .unwrap();

    assert!(combined.use_dynamic_port, "use_dynamic_port copied from the first source");
    assert_eq!(combined.fixed_port, Some(45001), "fixed_port transferred from the first source");

    let new_owner = format!("{}:{}", p.id, combined.id);
    let entries = ports.list();
    assert!(entries.iter().any(|e| e.owner == new_owner && e.port == 45001), "new command must hold the transferred port");
    assert!(!entries.iter().any(|e| e.owner == owner1), "the old owner's reservation must be released");
}

#[test]
fn combine_commands_flutter_template_keeps_flutter_kind() {
    let dir = tempfile::tempdir().unwrap();
    let sup = new_sup(dir.path());
    let p = sup.add_project("My App".into(), "C:/tmp".into()).unwrap();

    let src1 = sup
        .add_command(&p.id, "chrome".into(), "fvm flutter run -d chrome".into(), None, false, false, None, "".into(), None, false, Vec::new())
        .unwrap()
        .command;
    assert_eq!(src1.kind, ProcKind::Flutter);
    let src2 = sup
        .add_command(&p.id, "web-server".into(), "fvm flutter run -d web-server".into(), None, false, false, None, "".into(), None, false, Vec::new())
        .unwrap()
        .command;

    let combined = sup
        .combine_commands(
            &p.id,
            vec![src1.id.clone(), src2.id.clone()],
            "Run".into(),
            "fvm flutter run {DEVICE}".into(),
            CommandParam {
                name: "device".to_string(),
                label: "Device".to_string(),
                values: vec![
                    ParamValue { value: "chrome".to_string(), label: "Chrome".to_string(), flag: "-d chrome".to_string() },
                    ParamValue { value: "web-server".to_string(), label: "Web Server".to_string(), flag: "-d web-server".to_string() },
                ],
                last_value: None,
            },
        )
        .unwrap();

    assert_eq!(combined.kind, ProcKind::Flutter, "a flutter template must keep the Flutter kind");
}

#[test]
fn combine_commands_inserts_the_new_command_at_the_first_sources_index() {
    let dir = tempfile::tempdir().unwrap();
    let sup = new_sup(dir.path());
    let p = sup.add_project("My App".into(), "C:/tmp".into()).unwrap();

    let a = sup.add_command(&p.id, "a".into(), "echo a".into(), None, false, false, None, "".into(), None, false, Vec::new()).unwrap().command;
    let b = sup.add_command(&p.id, "b".into(), "echo b".into(), None, false, false, None, "".into(), None, false, Vec::new()).unwrap().command;
    let c = sup.add_command(&p.id, "c".into(), "echo c".into(), None, false, false, None, "".into(), None, false, Vec::new()).unwrap().command;
    let d = sup.add_command(&p.id, "d".into(), "echo d".into(), None, false, false, None, "".into(), None, false, Vec::new()).unwrap().command;

    // Picked in reverse order (C then A): the FIRST selected source is C, but
    // A sits earlier in project order, so removing it shifts everything after
    // it left by one before the new command lands.
    sup
        .combine_commands(
            &p.id,
            vec![c.id.clone(), a.id.clone()],
            "Run".into(),
            "echo {X}".into(),
            CommandParam {
                name: "x".to_string(),
                label: "X".to_string(),
                values: vec![
                    ParamValue { value: "cv".to_string(), label: "c".to_string(), flag: "c".to_string() },
                    ParamValue { value: "av".to_string(), label: "a".to_string(), flag: "a".to_string() },
                ],
                last_value: None,
            },
        )
        .unwrap();

    let names: Vec<String> = sup.list_projects()[0].commands.iter().map(|c| c.name.clone()).collect();
    assert_eq!(names, vec!["b".to_string(), "Run".to_string(), "d".to_string()], "combined command lands between the surviving neighbours of the first source");
    let _ = (b.id, d.id); // surviving sources, referenced only by name above
}
