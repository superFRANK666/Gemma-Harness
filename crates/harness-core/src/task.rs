//! Durable, bounded task notes. These are model-authored claims, not user instructions.
use crate::store::Store;
use anyhow::{bail, Result};
use serde_json::{json, Value};

pub fn definition() -> Value {
    let text = |max| json!({"type":"string","minLength":1,"maxLength":max});
    json!({
        "name":"task_checkpoint",
        "description":"Save or replace a concise task checkpoint for multi-step work. Survives context compaction and restarts. Align it with actual user instructions; skip for simple questions. Completed steps need a result. For evidence, copy evidence_call_id from a successful tool result into call_id; never guess IDs. This records claims, not proof that those claims follow from evidence. Mark blocked with a concrete blocker when user input or external changes are necessary. Do not store private chain-of-thought; record conclusions, uncertainty and the next action.",
        "parameters":{
            "type":"object","additionalProperties":false,
            "properties":{
                "objective":text(240),
                "status":{"type":"string","enum":["active","blocked","completed"]},
                "steps":{"type":"array","minItems":1,"maxItems":8,"items":{
                    "type":"object","additionalProperties":false,
                    "properties":{
                        "title":text(120),
                        "status":{"type":"string","enum":["pending","in_progress","blocked","completed"]},
                        "result":{"type":"string","maxLength":240}
                    },"required":["title","status"]
                }},
                "evidence":{"type":"array","maxItems":6,"items":{
                    "type":"object","additionalProperties":false,
                    "properties":{"call_id":text(128),"claim":text(240)},
                    "required":["call_id","claim"]
                }},
                "uncertainties":{"type":"array","maxItems":4,"items":text(160)},
                "next_action":{"type":"string","maxLength":160},
                "blocker":{"type":"string","maxLength":240},
                "report_only":{"type":"boolean","description":"True only for a user-requested status report or clarification while the underlying task stays active. Applies to this turn only; preserve its original objective and unfinished steps."}
            },"required":["objective","status","steps","evidence","uncertainties","next_action"]
        }
    })
}

pub fn failed(result: &Value) -> bool {
    result["isError"] == true
        || result["indeterminate"] == true
        || result["timed_out"] == true
        || result["cancelled"] == true
        || result["exit_code"].as_i64().is_some_and(|code| code != 0)
}

pub fn save(store: &Store, session: &str, state: Value) -> Result<Value> {
    // Schema validation happens before dispatch; semantic checks happen here.
    if state.to_string().chars().count() > 4200 {
        bail!("Task checkpoint exceeds 4200 characters; shorten conclusions and evidence")
    }
    let steps = state["steps"].as_array().unwrap();
    if state["objective"].as_str().unwrap().trim().is_empty()
        || steps
            .iter()
            .any(|step| step["title"].as_str().unwrap().trim().is_empty())
    {
        bail!("Objective and step titles must contain meaningful text")
    }
    if state["report_only"] == true && state["status"] != "active" {
        bail!("report_only is for reporting an active task without marking it completed")
    }
    for step in steps {
        if step["status"] == "completed"
            && step["result"].as_str().is_none_or(|s| s.trim().is_empty())
        {
            bail!("Every completed step needs a concise result and its verification limits")
        }
    }
    match state["status"].as_str().unwrap() {
        "completed" => {
            if steps.iter().any(|s| s["status"] != "completed")
                || !state["next_action"].as_str().unwrap().trim().is_empty()
                || state["blocker"]
                    .as_str()
                    .is_some_and(|s| !s.trim().is_empty())
            {
                bail!("Completed tasks must have completed steps, no blocker and no next action")
            }
        }
        "blocked" => {
            if state["blocker"]
                .as_str()
                .is_none_or(|s| s.trim().is_empty())
            {
                bail!("Blocked tasks need a concrete blocker")
            }
        }
        _ => {
            if state["next_action"].as_str().unwrap().trim().is_empty()
                || !steps
                    .iter()
                    .any(|s| matches!(s["status"].as_str(), Some("pending" | "in_progress")))
            {
                bail!("Active tasks need unfinished work and a concrete next action")
            }
        }
    }
    for evidence in state["evidence"].as_array().unwrap() {
        let call_id = evidence["call_id"].as_str().unwrap();
        let event = store.tool_evidence(session, call_id)?;
        if event.as_ref().is_none_or(|event| {
            event["name"] == "task_checkpoint"
                || event["result"].is_null()
                || failed(&event["result"])
                || event["result"]["requires_observation"] == true
        }) {
            bail!("Evidence call {call_id} is missing, ambiguous, failed, unobserved or self-referential; inspect real state")
        }
    }
    store.append(session, "task_checkpoint", &state)?;
    Ok(
        json!({"saved":true,"state":state,"note":"Model-authored checkpoint. Successful call IDs establish provenance only; verify that the evidence actually supports each claim. Original user instructions remain authoritative."}),
    )
}
