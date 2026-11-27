use crate::runtime::{
    Result,
    controller_events::{Event, Notify},
};
use rustix::{
    event::{PollFd, PollFlags, poll},
    fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags},
    io::Errno,
};
use std::{
    fs,
    mem::MaybeUninit,
    os::unix::{fs::DirBuilderExt, net::UnixStream},
    path::Path,
    thread,
};

/// Atomically published controls and entries can come from the decision
/// lane or an operator command in a separate process.
pub(super) struct ChoiceWatch {
    _stop: UnixStream,
}

impl ChoiceWatch {
    pub(super) fn start(root: &Path, notify: Notify) -> Result<Self> {
        let entries = root.join("entries");
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&entries)?;
        let fd = inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK)?;
        let control = inotify::add_watch(&fd, root, WatchFlags::MOVED_TO | WatchFlags::DELETE)?;
        let entries = inotify::add_watch(
            &fd,
            &entries,
            WatchFlags::CREATE | WatchFlags::MOVED_TO | WatchFlags::DELETE,
        )?;
        let (stop, stop_reader) = UnixStream::pair()?;
        thread::Builder::new()
            .name("strategy-choice".into())
            .spawn(move || {
                let mut buffer = [MaybeUninit::uninit(); 4096];
                let mut reader = inotify::Reader::new(&fd, &mut buffer);
                let mut poll_fds = [
                    PollFd::new(&fd, PollFlags::IN),
                    PollFd::new(&stop_reader, PollFlags::IN),
                ];
                loop {
                    match poll(&mut poll_fds, None) {
                        Ok(_) if !poll_fds[1].revents().is_empty() => break,
                        Ok(_) if poll_fds[0].revents().contains(PollFlags::IN) => {}
                        Err(Errno::INTR) => continue,
                        _ => {
                            eprintln!("strategy_choice_watch_failed");
                            break;
                        }
                    }
                    loop {
                        match reader.next() {
                            Ok(event) => {
                                let name = event.file_name().map(|name| name.to_bytes());
                                if event.events().contains(ReadFlags::QUEUE_OVERFLOW)
                                    || (event.wd() == control && name == Some(b"controls.json"))
                                    || (event.wd() == entries
                                        && name.is_some_and(|name| {
                                            name.ends_with(b".json") && !name.starts_with(b".")
                                        }))
                                {
                                    notify.signal(Event::Choice);
                                }
                            }
                            Err(Errno::AGAIN) => break,
                            Err(Errno::INTR) => continue,
                            Err(_) => {
                                eprintln!("strategy_choice_watch_failed");
                                return;
                            }
                        }
                    }
                }
            })?;
        Ok(Self { _stop: stop })
    }
}

#[cfg(test)]
mod tests;
