use super::*;
use crate::runtime::{
    controller_events::Inbox,
    strategy_library::{Library, choice::ChoicePolicy},
};
use std::time::Duration;

#[test]
fn published_stand_down_notifies_the_controller() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library");
    let inbox = Inbox::new();
    let _watcher = ChoiceWatch::start(&path, inbox.notify()).unwrap();
    Library::at(path)
        .select_by_lane(None, &ChoicePolicy::default(), "fixture", None, 1)
        .unwrap();
    assert!(inbox.wait(Duration::from_secs(1)).contains(Event::Choice));
}
