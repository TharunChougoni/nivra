//! The trading lane's only path into the research lane (#239).
use super::*;

#[test]
fn registered_research_waits_only_for_the_book_and_a_pending_owner_decision() {
    // #236: the launch gate takes no model-worker input, so a busy scorer,
    // aside, review or supervisor turn can never starve registered research.
    let ready = ResearchLaunch::for_book(LaunchBook::default());
    assert_eq!(
        ready,
        ResearchLaunch {
            registered: true,
            candidate: true,
            reason: "ready",
        }
    );
    // A queued or in-flight owner decision still keeps registered research
    // out of the way; candidate compute is unaffected, as before.
    let owner = ResearchLaunch::for_book(LaunchBook {
        decision_pending: true,
        ..Default::default()
    });
    assert!(!owner.registered && owner.candidate);
    assert_eq!(owner.reason, "owner_decision_pending");
    for (book, reason) in [
        (
            LaunchBook {
                halted: true,
                ..Default::default()
            },
            "book_halted",
        ),
        (
            LaunchBook {
                stopped: true,
                ..Default::default()
            },
            "book_stopped",
        ),
        (
            LaunchBook {
                context_unavailable: true,
                ..Default::default()
            },
            "controller_session_end_unavailable",
        ),
    ] {
        let launch = ResearchLaunch::for_book(book);
        assert!(!launch.registered && !launch.candidate, "{reason}");
        assert_eq!(launch.reason, reason);
    }
}
