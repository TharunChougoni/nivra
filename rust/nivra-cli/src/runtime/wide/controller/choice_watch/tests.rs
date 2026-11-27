use super::*;
use crate::runtime::{
    controller_events::Inbox,
    strategy_library::{Control, ControlRecord, Library, choice::ChoicePolicy},
};
use std::time::Duration;

#[test]
fn published_lane_and_owner_controls_notify_the_controller() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library");
    let inbox = Inbox::new();
    let _watcher = ChoiceWatch::start(&path, inbox.notify()).unwrap();
    let library = Library::at(path);
    library
        .select_by_lane(None, &ChoicePolicy::default(), "fixture", None, 1)
        .unwrap();
    assert!(inbox.wait(Duration::from_secs(1)).contains(Event::Choice));
    library
        .append(ControlRecord {
            at_ms: 2,
            control: Control::Unpin,
        })
        .unwrap();
    assert!(inbox.wait(Duration::from_secs(1)).contains(Event::Choice));
}
