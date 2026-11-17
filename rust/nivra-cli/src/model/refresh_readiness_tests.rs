use super::*;
#[test]
fn refresh_failure_notifies_immediately_and_only_verified_success_covers() {
    let at = chrono::DateTime::parse_from_rfc3339("2026-10-26T00:00:00Z").unwrap();
    let initial = json!({"state":"authenticated","grant_id":"a".repeat(32),"revision":1,"login_attempts":1,"updated_at":at,"expires_at":at,"account_hash":hash(b"synthetic-account"),"access_token":"synthetic-access","refresh_token":"synthetic-refresh"});
    let discovery = json!({"issuer":"https://auth.x.ai","device_authorization_endpoint":"https://auth.x.ai/oauth2/device/code","token_endpoint":"https://auth.x.ai/oauth2/token","userinfo_endpoint":"https://auth.x.ai/oauth2/userinfo","grant_types_supported":["urn:ietf:params:oauth:grant-type:device_code","refresh_token"],"token_endpoint_auth_methods_supported":["none"],"scopes_supported":SCOPE.split_whitespace().collect::<Vec<_>>()});
    for failure in ["none", "discovery", "token", "identity", "persist", "fence"] {
        let mut stored = serde_json::to_vec(&initial).unwrap();
        let mut notices = 0;
        let mut calls = 0;
        let mut writes = 0;
        let result = refresh_with(
            stored.clone(),
            at,
            |_, _, _, _| {
                calls += 1;
                match calls {
                    1 if failure == "discovery" => Err("synthetic-provider-secret".into()),
                    1 => Ok((200, serde_json::to_vec(&discovery).unwrap())),
                    2 if failure == "token" => Ok((401, br#"{"error":"synthetic-refresh"}"#.to_vec())),
                    2 => Ok((200, br#"{"access_token":"synthetic-rotated","expires_in":3600}"#.to_vec())),
                    3 => Ok((200, serde_json::to_vec(&json!({"sub":if failure == "identity" {"other"} else {"synthetic-account"}})).unwrap())),
                    _ => panic!("unexpected request"),
                }
            },
            |old, new| {
                writes += 1;
                assert_eq!(old, stored);
                if failure == "fence" || (failure == "persist" && writes == 2) {
                    return Err(());
                }
                stored = new.to_vec();
                Ok(())
            },
            |details| {
                assert_eq!(
                    details,
                    json!({"provider":"xai","reason":"refresh_failed","action":"operator_review_required"})
                );
                notices += 1;
            },
        );
        assert_eq!(notices, usize::from(failure != "none"));
        let g = native::decode(&stored).unwrap();
        assert_eq!(result.is_ok(), failure == "none");
        assert_eq!(verified_refresh_covers(&g, at), failure == "none");
        if failure == "none" {
            GrantRecord::parse(&stored).unwrap();
            assert!(!verified_refresh_covers(
                &g,
                at - chrono::Duration::seconds(1)
            ));
            assert!(verified_refresh_covers(&g, at + chrono::Duration::hours(2)));
            assert!(verified_refresh_covers(&g, at + REFRESH_EVIDENCE_MAX_AGE));
            assert!(!verified_refresh_covers(
                &g,
                at + REFRESH_EVIDENCE_MAX_AGE + chrono::Duration::seconds(1)
            ));
            let pending = refresh_pending(&g, at).unwrap();
            assert!(!verified_refresh_covers(&pending, at));
            assert!(pending.get("refresh_verified_at").is_none());
            let mut unverified = g.clone();
            unverified
                .as_object_mut()
                .unwrap()
                .remove("refresh_verified_at");
            assert!(!verified_refresh_covers(&unverified, at));
        } else if failure != "fence" {
            assert_eq!(g["state"], "refreshing");
            assert!(
                refresh_with(
                    stored,
                    at,
                    |_, _, _, _| panic!("no retry"),
                    |_, _| panic!("no rewrite"),
                    |_| panic!("no new attempt")
                )
                .is_err()
            );
        }
    }
}
