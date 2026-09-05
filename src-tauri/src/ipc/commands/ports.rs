use crate::ports::{PortEntry, PortRegistry};
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub fn list_ports(reg: State<Arc<PortRegistry>>) -> Vec<PortEntry> {
    reg.list()
}

#[tauri::command]
pub fn reserve_port(reg: State<Arc<PortRegistry>>, owner: String) -> u16 {
    reg.reserve_next(&owner)
}
