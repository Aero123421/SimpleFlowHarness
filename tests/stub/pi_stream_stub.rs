//! Deterministic `pi --mode json` stand-in for oversized-stream regression tests.
//! It emits more than sfh's raw transcript cap, with usage before and after the
//! noisy middle and the authoritative assistant answer at the very end.

use std::io::{Read, Write};

const NOISE_BYTES: usize = 34 * 1024 * 1024;
const OVERSIZED_LINE_BYTES: usize = 17 * 1024 * 1024;

fn json_string(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let session = args
        .windows(2)
        .find(|pair| pair[0] == "--session-id")
        .map(|pair| pair[1].clone())
        .unwrap_or_else(|| "pi-stream-stub".to_string());

    let mut prompt = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut prompt);

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(
        out,
        "{{\"type\":\"session\",\"id\":{},\"timestamp\":\"stream-marker\"}}",
        json_string(&session)
    )
    .unwrap();
    writeln!(
        out,
        "{{\"type\":\"message_end\",\"message\":{{\"role\":\"assistant\",\"stopReason\":\"toolUse\",\"content\":[],\"usage\":{{\"input\":10,\"output\":2,\"cost\":{{\"total\":0.25}}}}}}}}"
    )
    .unwrap();

    if std::env::var_os("SFH_PI_STUB_OVERSIZED_LINE").is_some() {
        let payload = "x".repeat(OVERSIZED_LINE_BYTES);
        writeln!(
            out,
            "{{\"type\":\"message_update\",\"partial\":{{\"payload\":\"{payload}\"}}}}"
        )
        .unwrap();
    } else {
        let payload = "x".repeat(64 * 1024);
        let noise = format!(
            "{{\"type\":\"message_update\",\"partial\":{{\"payload\":\"{payload}\"}}}}\n"
        );
        let mut written = 0usize;
        while written < NOISE_BYTES {
            out.write_all(noise.as_bytes()).unwrap();
            written = written.saturating_add(noise.len());
        }
    }

    writeln!(
        out,
        "{{\"type\":\"message_end\",\"message\":{{\"role\":\"assistant\",\"stopReason\":\"stop\",\"content\":[{{\"type\":\"text\",\"text\":\"VERDICT: PASS\"}}],\"usage\":{{\"input\":20,\"output\":3,\"cost\":{{\"total\":0.5}}}}}}}}"
    )
    .unwrap();
    out.flush().unwrap();
}
