use super::proxy;
use super::ManagedProc;
use crate::types::{EnvVar, ProcStatus};
use std::process::{Command, Stdio};

impl ManagedProc {
    /// Detect a process that exited on its own (crash or self-stop) and update status.
    pub fn refresh(&mut self) {
        if let Some(child) = self.child.as_mut() {
            if let Ok(Some(status)) = child.try_wait() {
                self.status = if status.success() {
                    ProcStatus::Stopped
                } else {
                    self.crashed_at = Some(super::now_ms());
                    ProcStatus::Crashed
                };
                self.pid = None;
                self.child = None;
                self.stdin = None;
                // No longer running: drop the cached RAM/CPU/port so the
                // dashboard doesn't show a frozen figure until the next sampler tick.
                self.sampled_mem = None;
                self.sampled_cpu_pct = None;
                self.sampled_port = None;
                // The run that owned this env just ended: drop it so a later
                // adopt (after a future restart) or a stopped view never shows
                // this run's stale values.
                self.resolved_env = None;
                // fallback_port, like acquired_port, is deliberately left as-is
                // here (not reset) - it mirrors the last run's port situation
                // until the next explicit start()/stop(), same as acquired_port
                // itself. Trivial/fast-exiting commands can reach this branch
                // within the EADDRINUSE retry window's own refresh() call, and
                // clearing it here would erase the flag before the dashboard
                // ever saw it.
                // The child died on its own. Drop the proxy so its TcpListener on
                // the public port is freed immediately (otherwise it keeps serving
                // 502s until an explicit stop/restart); dropping ProxyTask runs its
                // graceful stop + thread join. Also drop the reload sender. Do NOT
                // clear internal_port: reap_tick reads internal_port() to release
                // the registry entry, and clearing it here would leak that
                // bookkeeping.
                self.proxy = None;
                self.reload_tx = None;
            }
            return;
        }
        // Adopted: no Child to wait on. Poll the OS - if the wrapper PID is gone,
        // the process ended on its own; we can't know the exit code, so mark it
        // Stopped (neutral) and drop adoption.
        if self.adopted {
            if let Some(pid) = self.pid {
                if !super::super::reaper::pid_is_our_wrapper(pid) {
                    self.status = ProcStatus::Stopped;
                    self.pid = None;
                    self.adopted = false;
                    self.push_log("stdout", "[supervisor] re-adopted process exited".to_string());
                }
            }
        }
    }

    /// Spawn the process via `cmd /C <cmd>` in its own process group so the whole
    /// tree can be killed later. Returns the spawned PID.
    ///
    /// `dynamic_port` is the port the child actually binds (the `{PORT}`
    /// substitution + PORT env), exactly as before. `proxy_public_port` is
    /// `Some(P)` only when this proc should sit behind a live-reload proxy: the
    /// child then binds `dynamic_port` (an internal ephemeral port) and the proxy
    /// fronts it on `P`, the port the dashboard advertises. `None` => no proxy.
    pub fn start(
        &mut self,
        dynamic_port: Option<u16>,
        proxy_public_port: Option<u16>,
    ) -> std::io::Result<u32> {
        self.refresh();
        if matches!(self.status, ProcStatus::Running | ProcStatus::Starting) {
            return Ok(self.pid.unwrap_or(0));
        }

        self.logs.lock().unwrap().clear();

        // Tear down any proxy left over from a prior (crashed) run before we
        // re-spawn, so the new proxy can re-bind the same public port. Dropping
        // it signals graceful shutdown + joins its thread.
        self.proxy = None;
        self.reload_tx = None;
        self.internal_port = None;

        // Stand up the live-reload proxy BEFORE spawning flutter so a bind failure
        // degrades cleanly. When the proxy binds, flutter binds an internal port
        // and the proxy fronts it on the public port. When it cannot bind, we run
        // flutter straight on the public port (no auto-reload) rather than leaving
        // an advertised-but-dead port. `child_port` is the port flutter ends up
        // binding either way.
        let mut proxy_task: Option<proxy::ProxyTask> = None;
        let mut reload_tx: Option<tokio::sync::broadcast::Sender<()>> = None;
        let mut internal_for_proxy: Option<u16> = None;
        let child_port = match (proxy_public_port, dynamic_port) {
            (Some(public), Some(internal)) => {
                let (tx, _rx) = tokio::sync::broadcast::channel(16);
                match proxy::spawn(public, internal, tx.clone()) {
                    Ok(task) => {
                        proxy_task = Some(task);
                        reload_tx = Some(tx);
                        internal_for_proxy = Some(internal);
                        self.push_log(
                            "stdout",
                            format!(
                                "[supervisor] live-reload proxy on 127.0.0.1:{public} -> flutter :{internal}"
                            ),
                        );
                        Some(internal)
                    }
                    Err(e) => {
                        self.push_log(
                            "stderr",
                            format!("[supervisor] live-reload proxy failed to bind {public}: {e}; serving flutter directly on {public}, auto-reload off"),
                        );
                        Some(public)
                    }
                }
            }
            _ => dynamic_port,
        };

        // Apply the port override (no project files touched): substitute any
        // `{PORT}` placeholder, and for a recognized framework that did not
        // express its own port, append the right CLI port flag (best-effort
        // force - a flag beats a hardcoded config port). The PORT env var is also
        // set below. `ports_detect` reports whatever it actually bound, so this is
        // a convenience, not load-bearing.
        let cmd_str = match child_port {
            Some(p) => super::super::port_inject::resolve_port(&self.spec.cmd, &self.spec.kind, p),
            None => self.spec.cmd.clone(),
        };

        // Flutter: force `--machine` so the daemon speaks the JSON-RPC protocol.
        // Machine mode is what lets us drive an `app.restart` over stdin for a
        // fast hot restart (and later a browser-reload signal); without it the
        // daemon ignores our requests. The reader in spawn_reader humanizes the
        // resulting JSON-RPC stdout back into readable log lines.
        let cmd_str = super::super::flutter::inject_machine_flag(&cmd_str, &self.spec.kind);

        let mut command = Command::new("cmd");
        command
            .arg("/C")
            .arg(&cmd_str)
            .current_dir(&self.spec.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Build the child PATH from the persisted machine+user registry PATH so
        // per-user toolchains (node via nvm, cargo/rustup) resolve regardless of
        // how the supervisor itself was launched: a logon-autostarted supervisor
        // may inherit only a reduced PATH that lacks the user entries, and
        // children would otherwise inherit that stripped PATH. Fall back to our
        // own inherited PATH if the registry read fails. Then resolve symlinked
        // PATH directories to their real targets so a toolchain installed behind
        // a junction (notably nvm-windows: `C:\nvm4w\nodejs` -> `...\nvm\v<ver>`)
        // launches without the "untrusted mount point" traversal failure. Done
        // before the per-command env overrides so an explicit `PATH=` override
        // still wins.
        #[cfg(windows)]
        {
            let base = super::super::spawn_env::registry_merged_path()
                .or_else(|| std::env::var("PATH").ok());
            if let Some(path) = base {
                command.env("PATH", super::super::spawn_env::resolve_path_dirs(&path));
            }
        }

        // Per-command env overrides (applied before PORT so a dynamic port still
        // wins). A `PATH=` here is now a fallback/override on top of the resolved
        // PATH above, not the only way to reach a junction-installed toolchain.
        // Parsed once and kept (not re-derived) so `resolved_env` below reflects
        // exactly what was applied to the child - the resolved values, not the
        // raw unexpanded `spec.env` text.
        let env_pairs = super::super::spawn_env::parse_env(&self.spec.env);
        for (k, v) in &env_pairs {
            command.env(k, v);
        }
        let mut resolved_env: Vec<EnvVar> = env_pairs
            .into_iter()
            .map(|(k, v)| EnvVar::new(k, v))
            .collect();

        if let Some(p) = child_port {
            command.env("PORT", p.to_string()); // env channel for process.env.PORT tools
            resolved_env.push(EnvVar::new("PORT".to_string(), p.to_string()));
        }

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            // No console window: stdio is piped, so children (dev servers) never
            // need one. Without this every spawn flashes a terminal on Windows.
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
        }

        let mut child = command.spawn()?;
        let pid = child.id();

        // reload_tx was decided above (Some only when the proxy actually bound).
        self.reload_tx = reload_tx.clone();

        // Reset appId for the new run; stdout reader re-captures it.
        *self.app_id.lock().unwrap() = None;
        if let Some(out) = child.stdout.take() {
            super::super::proc_log::spawn_reader(
                out,
                "stdout",
                self.logs.clone(),
                Some(self.app_id.clone()),
                self.reload_tx.clone(),
            );
        }
        if let Some(err) = child.stderr.take() {
            super::super::proc_log::spawn_reader(err, "stderr", self.logs.clone(), None, None);
        }
        self.stdin = child.stdin.take();
        // The dashboard advertises the public port: when proxied that is the
        // proxy's port, not the internal port the child actually bound.
        self.acquired_port = proxy_public_port.or(dynamic_port);
        self.adopted = false; // a real Child supersedes any prior adoption

        // Store the proxy decided + spawned above. `internal_for_proxy` is Some
        // only when the proxy bound (flutter on the internal port); on a bind
        // failure flutter took the public port and there is no proxy to track.
        self.internal_port = internal_for_proxy;
        self.proxy = proxy_task;

        self.push_log("stdout", format!("[supervisor] started: {cmd_str}"));
        self.child = Some(child);
        self.pid = Some(pid);
        self.started_at = Some(super::now_ms());
        self.crashed_at = None;
        self.status = ProcStatus::Running;
        self.resolved_env = Some(resolved_env);
        Ok(pid)
    }
}
