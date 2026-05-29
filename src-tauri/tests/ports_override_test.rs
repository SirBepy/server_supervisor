//! Proves the port override actually reaches a real server: a tiny node script
//! that binds process.env.PORT, started via the supervisor with useDynamicPort,
//! ends up listening on the acquired port.

use server_supervisor_lib::ports::PortRegistry;
use server_supervisor_lib::supervisor::Supervisor;
use std::sync::Arc;
use std::time::Duration;

fn write_node_project(dir: &std::path::Path) {
    let root = dir.display().to_string().replace('\\', "/");
    // Server prints the port it bound so we can assert from logs.
    std::fs::write(
        dir.join("server.js"),
        "const p=process.env.PORT;require('http').createServer((_,r)=>r.end('ok')).listen(p,()=>console.log('LISTENING '+p));",
    )
    .unwrap();
    let json = format!(
        r#"[{{"id":"np","name":"np","root":"{root}","commands":[{{"id":"web","name":"web","cmd":"node server.js","kind":"generic","autostart":false,"use_dynamic_port":true}}]}}]"#
    );
    std::fs::write(dir.join("projects.json"), json).unwrap();
}

#[test]
fn dynamic_port_env_reaches_node_server() {
    let dir = tempfile::tempdir().unwrap();
    write_node_project(dir.path());
    let ports = Arc::new(PortRegistry::new(dir.path().to_path_buf()));
    let sup = Supervisor::new(dir.path().to_path_buf(), ports);

    sup.start("np:web").unwrap();
    std::thread::sleep(Duration::from_millis(1800));

    let info = sup.list().into_iter().find(|p| p.id == "np:web").unwrap();
    let port = info.port.expect("a dynamic port should be assigned");
    assert!((42000..49000).contains(&port));

    let logs = sup.logs("np:web").unwrap();
    let bound = logs.iter().any(|l| l.text.contains(&format!("LISTENING {port}")));
    assert!(bound, "node server should bind the injected PORT env; logs: {logs:?}");

    sup.stop("np:web").unwrap();
}

/// Proves `{PORT}` substitution into the command string works independent of the
/// PORT env channel: the script reads its port from argv (the substituted `{PORT}`
/// flag), NOT process.env.PORT, and prints it. Asserts logs contain `FLAG <port>`
/// where `<port>` equals the registry-assigned `info.port`.
///
/// The script lives in a `.js` file rather than an inline `node -e "..."` because
/// the supervisor spawns via `cmd /C <cmd>`, and `cmd` strips the inner double
/// quotes of an `-e` payload (breaking the JS literal). Passing `{PORT}` as the
/// argv flag still exercises command-string substitution, which is the point.
#[test]
fn dynamic_port_flag_substitution_reaches_node_server() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().display().to_string().replace('\\', "/");
    // Reads the port from argv[2] (the {PORT}-substituted flag), ignoring env.
    std::fs::write(
        dir.path().join("flag_server.js"),
        "const p=process.argv[2];require('http').createServer((_,r)=>r.end()).listen(p,()=>console.log('FLAG '+p));",
    )
    .unwrap();
    let json = format!(
        r#"[{{"id":"fp","name":"fp","root":"{root}","commands":[{{"id":"web","name":"web","cmd":"node flag_server.js {{PORT}}","kind":"generic","autostart":false,"use_dynamic_port":true}}]}}]"#
    );
    std::fs::write(dir.path().join("projects.json"), json).unwrap();

    let ports = Arc::new(PortRegistry::new(dir.path().to_path_buf()));
    let sup = Supervisor::new(dir.path().to_path_buf(), ports);

    sup.start("fp:web").unwrap();
    std::thread::sleep(Duration::from_millis(1800));

    let info = sup.list().into_iter().find(|p| p.id == "fp:web").unwrap();
    let port = info.port.expect("a dynamic port should be assigned");
    assert!((42000..49000).contains(&port));

    let logs = sup.logs("fp:web").unwrap();
    let bound = logs.iter().any(|l| l.text.contains(&format!("FLAG {port}")));
    assert!(
        bound,
        "node server should bind the {{PORT}}-substituted argv flag; logs: {logs:?}"
    );

    sup.stop("fp:web").unwrap();
}
