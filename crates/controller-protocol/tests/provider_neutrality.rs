#[test]
fn protocol_source_contains_no_provider_vocabulary() {
    let source = include_str!("../src/lib.rs").to_ascii_lowercase();
    let forbidden = [
        "github",
        "webhook",
        "pull_request",
        "repository",
        "installation_token",
        "review_council",
        "review_finding",
    ];
    for term in forbidden {
        assert!(
            !source.contains(term),
            "controller protocol must not contain provider term {term:?}"
        );
    }
}
