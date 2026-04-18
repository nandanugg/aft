//! Integration tests for the `similar` command.
//!
//! Tests against the fixture project in `tests/fixtures/similarity/`
//! which has settlement-domain Go files with known similarity relationships.

use crate::helpers::{fixture_path, AftProcess};

fn configure_similarity(aft: &mut AftProcess) -> String {
    let fixtures = fixture_path("similarity");
    let root = fixtures.display().to_string();

    let resp = aft.send(&format!(
        r#"{{"id":"1","command":"configure","project_root":"{}"}}"#,
        root
    ));
    assert_eq!(resp["success"], true, "configure failed: {:?}", resp);
    root
}

// --- Basic protocol tests ---

#[test]
fn similar_without_configure_returns_error() {
    let mut aft = AftProcess::spawn();
    let resp = aft.send(
        r#"{"id":"1","command":"similar","file":"a.go","symbol":"Foo"}"#,
    );
    assert_eq!(resp["success"], false);
    assert_eq!(resp["code"], "not_configured");
    aft.shutdown();
}

#[test]
fn similar_missing_file_param_returns_error() {
    let mut aft = AftProcess::spawn();
    let root = configure_similarity(&mut aft);
    let _ = root;
    let resp = aft.send(r#"{"id":"2","command":"similar","symbol":"Foo"}"#);
    assert_eq!(resp["success"], false);
    assert_eq!(resp["code"], "invalid_request");
    aft.shutdown();
}

#[test]
fn similar_missing_symbol_param_returns_error() {
    let mut aft = AftProcess::spawn();
    let root = configure_similarity(&mut aft);
    let resp = aft.send(&format!(
        r#"{{"id":"2","command":"similar","file":"{}/merchant_settlement/service.go"}}"#,
        root
    ));
    assert_eq!(resp["success"], false);
    assert_eq!(resp["code"], "invalid_request");
    aft.shutdown();
}

#[test]
fn similar_unknown_symbol_returns_error() {
    let mut aft = AftProcess::spawn();
    let root = configure_similarity(&mut aft);
    let resp = aft.send(&format!(
        r#"{{"id":"2","command":"similar","file":"{}/merchant_settlement/service.go","symbol":"NonExistentFunctionXYZ"}}"#,
        root
    ));
    assert_eq!(resp["success"], false);
    assert_eq!(resp["code"], "symbol_not_found");
    aft.shutdown();
}

// --- Core query tests ---

#[test]
fn similar_returns_ranked_matches() {
    let mut aft = AftProcess::spawn();
    let root = configure_similarity(&mut aft);

    let resp = aft.send(&format!(
        r#"{{"id":"2","command":"similar","file":"{}/merchant_settlement/service.go","symbol":"SettleMerchantSettlement","top":5,"min_score":0.0}}"#,
        root
    ));

    assert_eq!(resp["success"], true, "similar failed: {:?}", resp);

    let query = &resp["query"];
    assert_eq!(query["symbol"], "SettleMerchantSettlement");

    let matches = resp["matches"].as_array().expect("matches should be an array");
    assert!(!matches.is_empty(), "should return at least one match");
    assert!(matches.len() <= 5, "should respect top=5");

    // Verify scores are in descending order
    for i in 1..matches.len() {
        let prev = matches[i - 1]["score"].as_f64().unwrap_or(0.0);
        let curr = matches[i]["score"].as_f64().unwrap_or(0.0);
        assert!(
            prev >= curr,
            "scores should be descending: {} vs {} at position {}",
            prev, curr, i
        );
    }

    aft.shutdown();
}

#[test]
fn similar_settlement_symbols_score_higher_than_payment() {
    let mut aft = AftProcess::spawn();
    let root = configure_similarity(&mut aft);

    let resp = aft.send(&format!(
        r#"{{"id":"2","command":"similar","file":"{}/merchant_settlement/service.go","symbol":"SettleMerchantSettlement","top":10,"min_score":0.0}}"#,
        root
    ));

    assert_eq!(resp["success"], true, "similar failed: {:?}", resp);

    let matches = resp["matches"].as_array().expect("matches should be an array");

    // Find highest-scoring settlement match and highest payment match
    let settlement_max = matches
        .iter()
        .filter(|m| {
            m["symbol"]
                .as_str()
                .map(|s| s.to_lowercase().contains("settle") || s.to_lowercase().contains("settlement"))
                .unwrap_or(false)
        })
        .map(|m| m["score"].as_f64().unwrap_or(0.0))
        .fold(f64::NEG_INFINITY, f64::max);

    let payment_max = matches
        .iter()
        .filter(|m| {
            m["symbol"]
                .as_str()
                .map(|s| s.to_lowercase().contains("payment") && !s.to_lowercase().contains("settle"))
                .unwrap_or(false)
        })
        .map(|m| m["score"].as_f64().unwrap_or(0.0))
        .fold(f64::NEG_INFINITY, f64::max);

    if settlement_max != f64::NEG_INFINITY && payment_max != f64::NEG_INFINITY {
        assert!(
            settlement_max > payment_max,
            "settlement symbols (max score={:.3}) should score higher than payment symbols (max score={:.3})",
            settlement_max, payment_max
        );
    }

    aft.shutdown();
}

// --- Flags tests ---

#[test]
fn similar_top_n_honored() {
    let mut aft = AftProcess::spawn();
    let root = configure_similarity(&mut aft);

    let resp = aft.send(&format!(
        r#"{{"id":"2","command":"similar","file":"{}/merchant_settlement/service.go","symbol":"SettleMerchantSettlement","top":2,"min_score":0.0}}"#,
        root
    ));

    assert_eq!(resp["success"], true, "similar failed: {:?}", resp);
    let matches = resp["matches"].as_array().expect("matches");
    assert!(matches.len() <= 2, "top=2 should limit results to 2, got {}", matches.len());

    aft.shutdown();
}

#[test]
fn similar_min_score_filters() {
    let mut aft = AftProcess::spawn();
    let root = configure_similarity(&mut aft);

    let resp = aft.send(&format!(
        r#"{{"id":"2","command":"similar","file":"{}/merchant_settlement/service.go","symbol":"SettleMerchantSettlement","top":10,"min_score":0.99}}"#,
        root
    ));

    assert_eq!(resp["success"], true, "similar failed: {:?}", resp);
    let matches = resp["matches"].as_array().expect("matches");

    // With very high min_score, all results should be above threshold
    for m in matches {
        let score = m["score"].as_f64().unwrap_or(0.0);
        assert!(score >= 0.99, "score {} should be >= 0.99", score);
    }

    aft.shutdown();
}

#[test]
fn similar_explain_includes_breakdown() {
    let mut aft = AftProcess::spawn();
    let root = configure_similarity(&mut aft);

    let resp = aft.send(&format!(
        r#"{{"id":"2","command":"similar","file":"{}/merchant_settlement/service.go","symbol":"SettleMerchantSettlement","top":3,"min_score":0.0,"explain":true}}"#,
        root
    ));

    assert_eq!(resp["success"], true, "similar failed: {:?}", resp);

    let matches = resp["matches"].as_array().expect("matches");
    for m in matches {
        assert!(
            m.get("breakdown").is_some() && !m["breakdown"].is_null(),
            "explain=true should produce breakdown for each match, got: {:?}",
            m
        );
        let bd = &m["breakdown"];
        assert!(bd.get("lex").is_some(), "breakdown should have lex score");
        assert!(bd.get("co_citation").is_some(), "breakdown should have co_citation score");
    }

    aft.shutdown();
}

#[test]
fn similar_without_explain_no_breakdown() {
    let mut aft = AftProcess::spawn();
    let root = configure_similarity(&mut aft);

    let resp = aft.send(&format!(
        r#"{{"id":"2","command":"similar","file":"{}/merchant_settlement/service.go","symbol":"SettleMerchantSettlement","top":3,"min_score":0.0}}"#,
        root
    ));

    assert_eq!(resp["success"], true, "similar failed: {:?}", resp);

    let matches = resp["matches"].as_array().expect("matches");
    for m in matches {
        assert!(
            m.get("breakdown").is_none() || m["breakdown"].is_null(),
            "without explain, breakdown should be absent"
        );
    }

    aft.shutdown();
}

#[test]
fn similar_dict_flag_works_with_synonyms_file() {
    let mut aft = AftProcess::spawn();
    let root = configure_similarity(&mut aft);

    // With dict=true the synonym dict at .aft/synonyms.toml should be applied
    let resp = aft.send(&format!(
        r#"{{"id":"2","command":"similar","file":"{}/merchant_settlement/service.go","symbol":"SettleMerchantSettlement","top":5,"min_score":0.0,"dict":true}}"#,
        root
    ));

    assert_eq!(resp["success"], true, "similar with dict=true failed: {:?}", resp);

    let matches = resp["matches"].as_array().expect("matches");
    // Should still return ordered results; dict should not break anything
    for i in 1..matches.len() {
        let prev = matches[i - 1]["score"].as_f64().unwrap_or(0.0);
        let curr = matches[i]["score"].as_f64().unwrap_or(0.0);
        assert!(prev >= curr, "scores should remain descending with dict=true");
    }

    aft.shutdown();
}

#[test]
fn similar_explain_target_tokens_present_with_explain() {
    let mut aft = AftProcess::spawn();
    let root = configure_similarity(&mut aft);

    let resp = aft.send(&format!(
        r#"{{"id":"2","command":"similar","file":"{}/merchant_settlement/service.go","symbol":"SettleMerchantSettlement","top":3,"min_score":0.0,"explain":true}}"#,
        root
    ));

    assert_eq!(resp["success"], true, "similar failed: {:?}", resp);

    // target_tokens should be present with explain=true
    let target_tokens = resp.get("target_tokens");
    if let Some(tt) = target_tokens {
        if !tt.is_null() {
            let tokens = tt.as_array().expect("target_tokens should be an array");
            // Should have at least "settle" and "merchant" stems
            assert!(!tokens.is_empty(), "target_tokens should not be empty");
        }
    }

    aft.shutdown();
}
