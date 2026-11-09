//! Reproducible synthetic overhead probe, independent of model generation speed.
//! cargo run --release -p harness-core --example performance_probe
use anyhow::Result;
use harness_core::{
    context,
    store::{Session, Store},
};
use harness_model::ChatMessage;
use serde_json::{json, Value};
use std::{hint::black_box, time::Instant};

fn median_ms(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples[samples.len() / 2]
}
fn sample(mut operation: impl FnMut(), repeats: usize, iterations: usize) -> f64 {
    let mut times = Vec::new();
    for _ in 0..repeats {
        let t = Instant::now();
        for _ in 0..iterations {
            operation();
        }
        times.push(t.elapsed().as_secs_f64() * 1000.0 / iterations as f64);
    }
    median_ms(times)
}
fn main() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = Store::open(&temp.path().join("bench.sqlite"))?;
    let session = Session {
        id: "bench".into(),
        title: "synthetic".into(),
        workspace: "/unused".into(),
        device_id: "local".into(),
        connection_id: "unused".into(),
        created_at: "now".into(),
        updated_at: "now".into(),
    };
    store.create_session(&session)?;
    let output = json!({"type":"tool_output","text":"output ".repeat(600)});
    for i in 0..10000 {
        if i % 100 == 0 {
            store.append(
                "bench",
                "message",
                &json!({"role":"assistant","content":format!("older message {i}")}),
            )?;
        }
        store.append("bench", "activity", &output)?;
    }
    store.append(
        "bench",
        "context_compacted",
        &json!({"messages":[{"role":"user","content":"keep exact constraint"}]}),
    )?;
    for i in 0..10 {
        store.append(
            "bench",
            "message",
            &json!({"role":"assistant","content":format!("current message {i}")}),
        )?;
    }
    let old_replay = || {
        let mut messages: Vec<ChatMessage> = vec![];
        for e in store.events("bench").unwrap() {
            match e["kind"].as_str() {
                Some("context_compacted") => {
                    messages = serde_json::from_value(e["payload"]["messages"].clone()).unwrap()
                }
                Some("message") => {
                    messages.push(serde_json::from_value(e["payload"].clone()).unwrap())
                }
                _ => {}
            }
        }
        assert_eq!(messages.len(), 11);
        black_box(messages);
    };
    let replay_before = sample(old_replay, 7, 1);
    let replay_after = sample(
        || {
            let m = store.messages("bench").unwrap();
            assert_eq!(m.len(), 11);
            black_box(m);
        },
        7,
        100,
    );
    let messages = vec![
        ChatMessage::text("user", "inspect screenshot"),
        ChatMessage {
            role: "tool".into(),
            content: Some(
                json!([{"type":"image_url","image_url":{"url":format!("data:image/png;base64,{}","a".repeat(8*1024*1024))}}]),
            ),
            tool_calls: None,
            tool_call_id: Some("image".into()),
            name: Some("screenshot".into()),
            reasoning_content: None,
        },
    ];
    let old_estimate = || {
        let mut value = serde_json::to_value(&messages).unwrap();
        value[1]["content"][0]["image_url"] = json!({"url":"[binary image payload]"});
        let text = value.to_string() + "[]";
        black_box(
            text.bytes().filter(u8::is_ascii).count().div_ceil(3)
                + text.chars().filter(|c| !c.is_ascii()).count() * 2
                + 1536
                + 256,
        );
    };
    let estimate_before = sample(old_estimate, 7, 25);
    let estimate_after = sample(
        || {
            black_box(context::estimate(
                black_box(&messages),
                black_box(&Value::Array(vec![])),
            ));
        },
        7,
        10000,
    );
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "generatedAt":chrono::Utc::now().to_rfc3339(),"platform":std::env::consts::OS,"arch":std::env::consts::ARCH,
            "method":"release build; median of 7 batched samples; synthetic temporary SQLite; prior replay/estimator algorithms reproduced in same process",
            "harnessVersion":env!("CARGO_PKG_VERSION"),"modelGenerationMeasured":false,"history":{"activityEvents":10000,"textBytesPerActivity":4200,"projectedMessages":11,"beforeMs":replay_before,"afterMs":replay_after,"speedup":replay_before/replay_after},
            "imageBudgetEstimate":{"base64Bytes":8*1024*1024,"beforeMs":estimate_before,"afterMs":estimate_after,"speedup":estimate_before/estimate_after},
            "limits":"Microbenchmarks measure Harness overhead only. They do not establish Gemma quality, actual tokenizer accuracy, total app RSS, end-to-end TTFT or generation speed."
        }))?
    );
    Ok(())
}
