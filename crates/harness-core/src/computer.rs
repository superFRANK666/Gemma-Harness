//! Observation debt survives interruption; accepting input does not settle it.
use crate::{store::Store, task};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::collections::BTreeSet;

pub struct ComputerState {
    device: String,
    pending: BTreeSet<String>,
}
impl ComputerState {
    pub fn restore(store: &Store, session: &str, device: &str) -> Result<Self> {
        let saved = store.runtime_state(session, "computer_state")?;
        let mut pending = BTreeSet::new();
        if let Some(saved) = saved.filter(|saved| saved["device"] == device) {
            for group in saved["pending"].as_array().into_iter().flatten() {
                if let Some(group @ ("browser" | "desktop")) = group.as_str() {
                    pending.insert(group.into());
                }
            }
        }
        Ok(Self { device: device.into(), pending })
    }
    pub fn pending(&self) -> &BTreeSet<String> {
        &self.pending
    }
    fn persist(&self, store: &Store, session: &str) -> Result<()> {
        store.append(session, "computer_state", &json!({"device":self.device,"pending":self.pending}))?;
        Ok(())
    }
    pub fn begin(&mut self, store: &Store, session: &str, name: &str) -> Result<()> {
        let group = match name {
            "desktop_act" => "desktop",
            "browser_act" => "browser",
            _ => return Ok(()),
        };
        if self.pending.contains(group) {
            bail!("A prior {group} action needs observation. Call {group}_observe or a current screenshot before another action; do not repeat the input")
        }
        self.pending.insert(group.into());
        // Land before dispatch: even a process loss must not authorize blind retries.
        self.persist(store, session)
    }
    pub fn observe_result(
        &mut self,
        store: &Store,
        session: &str,
        name: &str,
        result: &Value,
        vision_ready: bool,
    ) -> Result<()> {
        if task::failed(result) {
            return Ok(());
        }
        let snapshot = result["snapshot_id"].as_str().is_some_and(|s| !s.is_empty());
        let group = match name {
            "desktop_observe" if snapshot => "desktop",
            "desktop_screenshot" if snapshot && vision_ready => "desktop",
            "browser_observe" if snapshot => "browser",
            "browser_screenshot" if snapshot && vision_ready => "browser",
            "browser_act" if snapshot || result["closed"] == true => "browser",
            _ => return Ok(()),
        };
        if self.pending.remove(group) {
            self.persist(store, session)?;
        }
        Ok(())
    }
}
