use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Default, Deserialize)]
pub struct JsonInput {
    pub schema_version: Option<u32>,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub kind_assertion: Option<String>,
    pub severity_assertion: Option<String>,
    pub expected_behavior: Option<String>,
    pub observed_behavior: Option<String>,
    pub reproduction: Option<String>,
    pub workaround: Option<String>,
    pub impact: Option<String>,
    pub confidence: Option<String>,
    pub sensitivity: Option<String>,
    pub labels: Option<Vec<String>>,
    // We will parse extra context/sources carefully inside report.rs
    pub idempotency_key: Option<String>,
    pub affected_repositories: Option<Vec<String>>,
}

#[derive(Debug, Default)]
pub struct ProseInput {
    pub title: String,
    pub summary: Option<String>,
    pub expected: Option<String>,
    pub observed: Option<String>,
    pub repro: Option<String>,
    pub workaround: Option<String>,
    pub impact: Option<String>,
}

pub fn parse_prose(text: &str) -> ProseInput {
    let mut input = ProseInput::default();
    
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return input;
    }

    let mut current_section = "Summary";
    let mut sections: HashMap<&str, Vec<&str>> = HashMap::new();

    let mut first_line_found = false;

    for line in lines {
        let trimmed = line.trim();
        if !first_line_found {
            if trimmed.is_empty() {
                continue;
            }
            input.title = trimmed.to_string();
            first_line_found = true;
            continue;
        }

        match trimmed {
            "Expected:" => current_section = "Expected",
            "Observed:" => current_section = "Observed",
            "Reproduction:" => current_section = "Reproduction",
            "Workaround:" => current_section = "Workaround",
            "Impact:" => current_section = "Impact",
            _ => {
                sections.entry(current_section).or_default().push(line);
            }
        }
    }

    let join_section = |name: &str| -> Option<String> {
        sections.get(name).map(|v| v.join("\n").trim().to_string()).filter(|s| !s.is_empty())
    };

    input.summary = join_section("Summary");
    input.expected = join_section("Expected");
    input.observed = join_section("Observed");
    input.repro = join_section("Reproduction");
    input.workaround = join_section("Workaround");
    input.impact = join_section("Impact");

    // If there are no structured sections, all content goes to observed_behavior instead of summary if we prefer?
    // The requirement says: "remaining text: summary or observed behavior;"
    // We'll just map it to summary.

    input
}
