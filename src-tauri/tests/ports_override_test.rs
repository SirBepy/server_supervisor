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
