//! Content-addressed task and job keys.
use super::*;

pub fn scoped_task_id(book_binding: &str, session: &str, task_key: &str) -> String {
    sha256_hex(format!("owner-research-task-v1:{book_binding}:{session}:{task_key}").as_bytes())
}
pub fn scoped_job_id(task_id: &str, revision: u32) -> String {
    sha256_hex(format!("owner-research-job-v1:{task_id}:{revision}").as_bytes())
}
