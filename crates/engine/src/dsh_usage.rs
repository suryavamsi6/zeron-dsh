//! DSH's own token meter, read read-only over its app gateway.
//!
//! `dsh-token-meter` keeps its accounting as session projections: there is no
//! usage endpoint and no aggregate, so the meter is read where it lives —
//! `session/list` for the sessions and its per-session `tokenUsage` summary,
//! then `session/projections` for one session's exact values. Both answers are
//! parsed here so the settings surface never guesses at a wire field.

use serde_json::{json, Value};

/// The four disjoint buckets DSH records per session. Reasoning tokens are
/// already inside `output`, so they are never counted twice.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenTotals {
    pub uncached_input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl TokenTotals {
    /// Every token the meter accounted, across the four buckets.
    pub fn total(&self) -> u64 {
        self.uncached_input + self.output + self.cache_read + self.cache_write
    }

    pub fn is_zero(&self) -> bool {
        self.total() == 0
    }
}

/// Approximate context occupancy of the newest request, as DSH reports it.
/// Each field is a last-wins observation of a different moment, so a model
/// switch can pair a fresh capacity with the previous route's usage.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContextPressure {
    pub pressure_tokens: Option<u64>,
    pub projected_tokens: Option<u64>,
    pub context_window: Option<u64>,
}

/// One session's metered usage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionUsage {
    pub session_id: String,
    /// When the session last wrote, in milliseconds since the epoch.
    pub updated_at: Option<i64>,
    /// The meter's cumulative totals; zero when no provider reported usage.
    pub totals: TokenTotals,
    pub context: ContextPressure,
    /// Whether the value came from the list summary rather than an exact
    /// projection read, so the surface can say how fresh it is.
    pub from_summary: bool,
}

/// The `session/list` params. Its Remote takes its request under `_request`,
/// which is the gateway's argument name, not a wire field of the answer.
pub fn session_list_params() -> Value {
    json!({ "_request": {} })
}

/// The `session/projections` params for one session.
pub fn projections_params(session_id: &str) -> Value {
    json!({ "request": { "sessionId": session_id } })
}

/// Parse `session/list`: the sessions DSH knows, with whatever usage the list
/// summary carries.
pub fn parse_sessions(value: &Value) -> Vec<SessionUsage> {
    let items = value["items"].as_array().or_else(|| value.as_array());
    items
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let session_id = item["sessionId"].as_str()?.to_string();
            let values = &item["projections"]["values"];
            Some(SessionUsage {
                session_id,
                updated_at: item["updatedAt"].as_i64(),
                totals: totals_of(&values["tokenUsage"]),
                context: context_of(&values["contextPressure"]),
                from_summary: true,
            })
        })
        .collect()
}

/// Parse `session/projections`: the exact values at the session's current log
/// cut. A missing session answers `null`; a deferred migration answers cache
/// hints, which are still that session's own recorded values.
pub fn parse_projections(value: &Value) -> Option<(TokenTotals, ContextPressure, bool)> {
    let values = &value["values"];
    if !values.is_object() {
        return None;
    }
    let exact = value["kind"].as_str() == Some("sequenced");
    Some((totals_of(&values["tokenUsage"]), context_of(&values["contextPressure"]), !exact))
}

/// Read the four buckets out of a `tokenUsage` projection, which is either the
/// view (the buckets themselves) or the stored state (`{totals, last}`).
fn totals_of(usage: &Value) -> TokenTotals {
    let buckets = if usage["totals"].is_object() { &usage["totals"] } else { usage };
    TokenTotals {
        uncached_input: buckets["uncachedInputTokens"].as_u64().unwrap_or(0),
        output: buckets["outputTokens"].as_u64().unwrap_or(0),
        cache_read: buckets["cacheReadTokens"].as_u64().unwrap_or(0),
        cache_write: buckets["cacheWriteTokens"].as_u64().unwrap_or(0),
    }
}

fn context_of(pressure: &Value) -> ContextPressure {
    ContextPressure {
        pressure_tokens: pressure["pressureTokens"].as_u64(),
        projected_tokens: pressure["projectedTokens"].as_u64(),
        context_window: pressure["contextWindow"].as_u64(),
    }
}

/// A compact token count for a settings row: `1.2K`, `48.3K`, `1.2M`, `912`.
pub fn token_label(tokens: u64) -> String {
    match tokens {
        0..=999 => tokens.to_string(),
        1_000..=999_999 => format!("{:.1}K", tokens as f64 / 1_000.0),
        _ => format!("{:.1}M", tokens as f64 / 1_000_000.0),
    }
}

/// The one-line usage summary a session row shows, or `None` when the meter
/// never recorded a sample for it (a blank session).
pub fn usage_summary(usage: &SessionUsage) -> Option<String> {
    if usage.totals.is_zero() {
        return None;
    }
    let totals = usage.totals;
    let mut line = format!(
        "{} in, {} out",
        token_label(totals.uncached_input),
        token_label(totals.output)
    );
    if totals.cache_read > 0 || totals.cache_write > 0 {
        line.push_str(&format!(
            ", {} cache read, {} cache write",
            token_label(totals.cache_read),
            token_label(totals.cache_write)
        ));
    }
    if let (Some(pressure), Some(window)) = (usage.context.pressure_tokens, usage.context.context_window)
        && window > 0
    {
        line.push_str(&format!(
            " · {} of {} context",
            token_label(pressure),
            token_label(window)
        ));
    }
    Some(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_summary_carries_each_sessions_totals() {
        let list = json!({ "items": [
            { "sessionId": "ses_1", "updatedAt": 1_700_000_000_000i64, "projections": { "values": {
                "tokenUsage": { "uncachedInputTokens": 1200, "outputTokens": 340, "cacheReadTokens": 20_000, "cacheWriteTokens": 0 },
                "contextPressure": { "pressureTokens": 18_000, "contextWindow": 1_000_000 },
            } } },
            { "sessionId": "ses_2", "blank": true },
            { "updatedAt": 1 },
        ] });
        let sessions = parse_sessions(&list);
        assert_eq!(sessions.len(), 2, "an item without a session id is skipped: {sessions:?}");
        assert_eq!(sessions[0].session_id, "ses_1");
        assert_eq!(sessions[0].totals.uncached_input, 1200);
        assert_eq!(sessions[0].totals.cache_read, 20_000);
        assert_eq!(sessions[0].totals.total(), 21_540);
        assert_eq!(sessions[0].context.pressure_tokens, Some(18_000));
        assert_eq!(sessions[0].context.context_window, Some(1_000_000));
        assert!(sessions[0].from_summary);
        assert_eq!(sessions[1].totals, TokenTotals::default(), "a session with no sample reads as zero");
        assert!(usage_summary(&sessions[1]).is_none());
        assert!(parse_sessions(&json!({})).is_empty());
    }

    #[test]
    fn exact_projections_read_the_sequenced_baseline_and_its_hints() {
        let exact = json!({ "kind": "sequenced", "asOfSeq": 12, "values": {
            "tokenUsage": { "totals": { "uncachedInputTokens": 9, "outputTokens": 4, "cacheReadTokens": 0, "cacheWriteTokens": 1 } },
            "contextPressure": { "projectedTokens": 900 },
        } });
        let (totals, context, from_summary) = parse_projections(&exact).unwrap();
        assert_eq!(totals.total(), 14, "the stored state's totals are read, not its `last` sample");
        assert_eq!(context.projected_tokens, Some(900));
        assert_eq!(context.pressure_tokens, None);
        assert!(!from_summary);

        let hints = json!({ "kind": "migration-required", "values": {
            "tokenUsage": { "uncachedInputTokens": 1, "outputTokens": 2, "cacheReadTokens": 3, "cacheWriteTokens": 4 },
        } });
        let (totals, _, from_summary) = parse_projections(&hints).unwrap();
        assert_eq!(totals.total(), 10);
        assert!(from_summary, "cache hints are the stored values, and say so");

        assert!(parse_projections(&Value::Null).is_none(), "a missing session answers null");
    }

    #[test]
    fn the_params_use_the_argument_names_the_gateway_validates() {
        assert_eq!(session_list_params(), json!({ "_request": {} }));
        assert_eq!(
            projections_params("ses_1"),
            json!({ "request": { "sessionId": "ses_1" } })
        );
    }

    #[test]
    fn tokens_are_labelled_compactly_and_the_row_reads_as_a_sentence() {
        assert_eq!(token_label(912), "912");
        assert_eq!(token_label(48_300), "48.3K");
        assert_eq!(token_label(1_240_000), "1.2M");
        let usage = SessionUsage {
            session_id: "ses_1".into(),
            updated_at: Some(0),
            totals: TokenTotals { uncached_input: 1_200, output: 340, cache_read: 20_000, cache_write: 0 },
            context: ContextPressure { pressure_tokens: Some(18_000), projected_tokens: None, context_window: Some(1_000_000) },
            from_summary: true,
        };
        assert_eq!(
            usage_summary(&usage).as_deref(),
            Some("1.2K in, 340 out, 20.0K cache read, 0 cache write · 18.0K of 1.0M context")
        );
    }
}
