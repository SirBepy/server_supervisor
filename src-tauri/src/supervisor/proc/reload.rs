use super::ManagedProc;
use crate::types::ProcKind;
use std::io::Write;

impl ManagedProc {
    /// Hot reload / restart a Flutter process by writing an `app.restart` message
    /// to the `flutter run --machine` daemon's stdin. Web uses `full=true` because
    /// hot reload is upstream-broken there.
    pub fn reload(&mut self, full: bool) -> Result<(), String> {
        if self.spec.kind != ProcKind::Flutter {
            return Err("reload is only supported for flutter processes".to_string());
        }
        let app_id = self
            .app_id
            .lock()
            .unwrap()
            .clone()
            .ok_or("flutter daemon not ready yet (no appId seen on stdout)")?;
        let stdin = self
            .stdin
            .as_mut()
            .ok_or("process has no stdin handle (not running?)")?;
        let msg = format!(
            "[{{\"id\":0,\"method\":\"app.restart\",\"params\":{{\"appId\":\"{}\",\"fullRestart\":{}}}}}]\n",
            app_id, full
        );
        stdin.write_all(msg.as_bytes()).map_err(|e| e.to_string())?;
        stdin.flush().map_err(|e| e.to_string())?;
        self.push_log(
            "stdout",
            format!("[supervisor] sent app.restart fullRestart={full}"),
        );
        Ok(())
    }
}
