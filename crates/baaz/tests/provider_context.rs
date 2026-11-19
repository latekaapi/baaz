//! docs/82 5.5: what Claude Code and Codex report about their sessions,
//! from the recorded fixtures. The context their next request carries,
//! against the model's window, is what the meter draws (docs/81 F D6;
//! before, the meter summed every turn's tokens and called it context);
//! Claude Code's own skills are what its `/` menu lists (F H).

use provider::facts::ContextReading;
use provider::ProviderEvent;

fn fixture(kind: &str, name: &str) -> Vec<String> {
    let path = format!("{}/../../fixtures/{kind}/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("fixture reads: {path}")).lines().map(str::to_owned).collect()
}

fn readings(events: Vec<ProviderEvent>) -> Vec<ContextReading> {
    events
        .into_iter()
        .filter_map(|event| match event {
            ProviderEvent::SessionFacts { side, .. } => side.context,
            _ => None,
        })
        .collect()
}

#[test]
fn claude_code_reports_the_last_requests_context_against_the_models_window() {
    let mut fold = provider_claude_code::fold::ClaudeFold::new();
    let mut events = Vec::new();
    for line in fixture("claude-code", "basic.jsonl") {
        provider_claude_code::fold::step_line(&mut fold, &line, &mut |event| events.push(event));
    }
    let readings = readings(events);
    assert_eq!(readings.len(), 1, "one reading per turn: {readings:?}");
    // input 10 + cache read 21448 + cache creation 8165 + output 57, of the
    // answering model's `contextWindow` 200000.
    assert_eq!(readings[0], ContextReading::measured(29_680, Some(200_000)));
}

#[test]
fn codex_reports_each_requests_context_against_the_models_window() {
    let mut fold = provider_codex::fold::CodexFold::new();
    let mut events = Vec::new();
    for line in fixture("codex", "basic.jsonl") {
        // The fixture wraps each wire line in its direction; the pump reads
        // the server's lines bare.
        let envelope: serde_json::Value = serde_json::from_str(&line).expect("envelope");
        if envelope["_dir"] == "server->client" {
            provider_codex::fold::step_line(&mut fold, &envelope["frame"].to_string(), &mut |event| events.push(event));
        }
    }
    let readings = readings(events);
    assert!(!readings.is_empty(), "the tokenUsage update is a reading");
    assert_eq!(readings.last(), Some(&ContextReading::measured(14_923, Some(258_400))));
}

#[test]
fn a_nearly_full_window_warns_and_nothing_blocks() {
    use provider::facts::ContextPressure;
    assert_eq!(ContextReading::measured(10, Some(100)).pressure, ContextPressure::Normal);
    assert_eq!(ContextReading::measured(90, Some(100)).pressure, ContextPressure::Warning);
    assert_eq!(ContextReading::measured(150, Some(100)).pressure, ContextPressure::Warning, "never Blocked: no grade was reported");
    assert_eq!(ContextReading::measured(10, Some(0)).window_tokens, None, "a zero window is no window");
}

/// docs/82 5.5 (docs/81 F H): Claude Code's `init` names the skills its
/// session can run as `/name`; the pump lists them for the `/` menu.
#[test]
fn claude_code_lists_its_own_skills_from_init() {
    let mut fold = provider_claude_code::fold::ClaudeFold::new();
    let mut listed = Vec::new();
    for line in fixture("claude-code", "basic.jsonl") {
        provider_claude_code::fold::step_line(&mut fold, &line, &mut |event| {
            if let ProviderEvent::SkillsListed { skills, .. } = event {
                listed.push(skills);
            }
        });
    }
    assert_eq!(listed.len(), 1, "one list, at init");
    let names: Vec<&str> = listed[0].iter().map(|skill| skill.name.as_str()).collect();
    assert!(names.contains(&"code-review") && names.contains(&"simplify"), "{names:?}");
}
