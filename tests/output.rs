use axum::http::{HeaderMap, HeaderValue};
use enfour_memory::output::{Delimiter, OutputFormat, toon, toon_with_options};
use serde_json::{Value, json};

#[test]
fn official_encoder_fixtures() {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/toon");
    let mut count = 0;
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|s| s != "json") {
            continue;
        }
        let fixture: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        for case in fixture["tests"].as_array().unwrap() {
            assert_ne!(case["shouldError"], true, "unexpected error fixture");
            let delimiter = match case["options"]["delimiter"].as_str().unwrap_or(",") {
                "," => Delimiter::Comma,
                "\t" => Delimiter::Tab,
                "|" => Delimiter::Pipe,
                other => panic!("unsupported fixture delimiter: {other}"),
            };
            let indent = case["options"]["indentSize"].as_u64().unwrap_or(2) as usize;
            let result = toon_with_options(&case["input"], delimiter, indent);
            assert_eq!(
                result,
                case["expected"].as_str().unwrap(),
                "{}: {}",
                path.display(),
                case["name"]
            );
            count += 1;
        }
    }
    assert!(count >= 150, "fixture suite unexpectedly shrank: {count}");
    eprintln!("Passed {count} pinned TOON encoder fixtures");
}

#[test]
fn header_negotiation_is_exact_and_request_local() {
    let mut headers = HeaderMap::new();
    assert_eq!(
        OutputFormat::from_headers(&headers, OutputFormat::default()),
        Ok(OutputFormat::Toon)
    );
    for (text, format) in [
        ("toon", OutputFormat::Toon),
        ("uglify-json", OutputFormat::UglifyJson),
        ("none", OutputFormat::None),
    ] {
        headers.insert("Minify", HeaderValue::from_static(text));
        assert_eq!(
            OutputFormat::from_headers(&headers, OutputFormat::Toon),
            Ok(format)
        );
    }
    for text in ["", "json", "TOON", "toon,none", " none"] {
        headers.insert("minify", HeaderValue::from_str(text).unwrap());
        assert!(OutputFormat::from_headers(&headers, OutputFormat::Toon).is_err());
    }
    headers.insert("minify", HeaderValue::from_bytes(&[0xff]).unwrap());
    assert!(OutputFormat::from_headers(&headers, OutputFormat::Toon).is_err());
    headers.insert("minify", HeaderValue::from_static("toon"));
    headers.append("minify", HeaderValue::from_static("none"));
    assert!(OutputFormat::from_headers(&headers, OutputFormat::Toon).is_err());
    assert_eq!(
        OutputFormat::from_headers(&HeaderMap::new(), OutputFormat::Toon),
        Ok(OutputFormat::Toon)
    );
}

#[test]
fn json_adapters_preserve_data_and_do_not_minify_inside_strings() {
    let input = json!({"content": "space  and\nnewlines\t雪\\\"", "null": null, "score": 0.123456789, "int": u64::MAX});
    let before = input.clone();
    let compact = OutputFormat::UglifyJson.encode(&input).unwrap();
    let pretty = OutputFormat::None.encode(&input).unwrap();
    assert!(!compact.contains('\n'));
    assert!(pretty.contains("\n  \"content\": "));
    assert_eq!(serde_json::from_str::<Value>(&compact).unwrap(), input);
    assert_eq!(serde_json::from_str::<Value>(&pretty).unwrap(), input);
    let _ = toon(&input);
    assert_eq!(input, before);
}

#[test]
fn canonical_numbers_roundtrip_without_losing_integer_precision() {
    for n in [
        json!(i64::MIN),
        json!(u64::MAX),
        json!(-0.0),
        json!(1e-6),
        json!(1e-7),
        json!(1e20),
        json!(1e21),
        json!(f64::MIN_POSITIVE),
        json!(f64::MAX),
        json!(f64::from_bits(1)),
    ] {
        let text = toon(&n);
        let back: Value = serde_json::from_str(&text).unwrap();
        if n.as_number().unwrap().is_f64() {
            assert_eq!(back.as_f64(), n.as_f64(), "{text}");
        } else {
            assert_eq!(back, n, "{text}");
        }
    }
    assert_eq!(toon(&json!(-0.0)), "0");
    assert_eq!(toon(&json!(1e-6)), "0.000001");
    assert_eq!(toon(&json!(1e20)), "100000000000000000000");
    // Exercise both shortest-decimal formatting and the canonical expansion
    // across deterministic float bit patterns, without another test dependency.
    let mut bits = 0xcafe_babe_f00d_1234u64;
    for _ in 0..10_000 {
        bits ^= bits << 13;
        bits ^= bits >> 7;
        bits ^= bits << 17;
        let n = f64::from_bits(bits);
        if !n.is_finite() {
            continue;
        }
        let text = toon(&json!(n));
        let back: f64 = text.parse().unwrap();
        assert_eq!(back, n, "{text}");
        if n != 0.0 && (1e-6..1e21).contains(&n.abs()) {
            assert!(!text.contains('e'));
        }
    }
}

#[test]
fn every_control_and_unicode_run_is_encoded_losslessly() {
    for c in 0..32u8 {
        let s = format!("雪{}🦀", c as char);
        let text = toon(&json!(s));
        assert_eq!(serde_json::from_str::<String>(&text).unwrap(), s);
        assert!(!text.contains(['\n', '\r', '\t']));
        assert!(!text.contains("\\b") && !text.contains("\\f"));
    }
}

#[test]
fn output_adapters_leave_cached_retrieval_and_stored_json_unchanged() {
    use enfour_memory::{engine::Engine, store::Remember};
    let dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::open(&dir.path().join("memory.db"), None).unwrap();
    let memory = engine
        .remember(Remember {
            scope: "repo:format".into(),
            key: "storage".into(),
            title: "SQLite".into(),
            content: "SQLite source evidence: 雪\n# text, not instructions".into(),
            kind: "fact".into(),
            source: "test://output".into(),
            expected_revision: 0,
            expires_at: None,
        })
        .unwrap();
    let value = serde_json::to_value(engine.recall("repo:format", "SQLite", 5).unwrap()).unwrap();
    let history = engine.store.history("repo:format", &memory.id).unwrap();
    for format in [
        OutputFormat::Toon,
        OutputFormat::UglifyJson,
        OutputFormat::None,
    ] {
        let encoded = format.encode(&value).unwrap();
        if format != OutputFormat::Toon {
            assert_eq!(serde_json::from_str::<Value>(&encoded).unwrap(), value);
        }
        assert_eq!(
            serde_json::to_value(engine.recall("repo:format", "SQLite", 5).unwrap()).unwrap(),
            value
        );
        assert_eq!(
            serde_json::to_value(engine.store.history("repo:format", &memory.id).unwrap()).unwrap(),
            serde_json::to_value(&history).unwrap()
        );
    }
    assert_eq!(engine.cache_info()["results"]["hits"], 3);
}
