//! GUI-independent queue state; native controls only display this model.
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use dictate_core::batch_transcription::{BatchJob, BatchUpdate, FileStatus, path_key};

pub struct Row {
    pub job: BatchJob,
    pub status: FileStatus,
}

#[derive(Default)]
pub struct Queue {
    pub rows: Vec<Row>,
    pub running: bool,
    next_id: u64,
    paths: HashSet<PathBuf>,
    previous: Vec<(u64, FileStatus)>,
    indices: HashMap<u64, usize>,
}

impl Queue {
    pub fn add(&mut self, path: PathBuf) -> bool {
        if !self.paths.insert(path_key(&path)) {
            return false;
        }
        self.next_id += 1;
        self.indices.insert(self.next_id, self.rows.len());
        self.rows.push(Row {
            job: BatchJob {
                id: self.next_id,
                path,
            },
            status: FileStatus::Pending,
        });
        true
    }

    pub fn can_retry(&self) -> bool {
        !self.running
            && self
                .rows
                .iter()
                .any(|row| matches!(row.status, FileStatus::Failed(_)))
    }

    /// Only queue entries are removed. IDs stay unique when a file is imported again.
    pub fn remove(&mut self, selected: &[usize]) -> Vec<u64> {
        let ids: HashSet<_> = selected
            .iter()
            .filter_map(|&index| self.rows.get(index).map(|row| row.job.id))
            .collect();
        let mut removed = Vec::with_capacity(ids.len());
        self.rows.retain(|row| {
            if ids.contains(&row.job.id) {
                self.paths.remove(&path_key(&row.job.path));
                removed.push(row.job.id);
                false
            } else {
                true
            }
        });
        self.previous.retain(|(id, _)| !ids.contains(id));
        self.indices.clear();
        self.indices.extend(
            self.rows
                .iter()
                .enumerate()
                .map(|(index, row)| (row.job.id, index)),
        );
        removed
    }

    pub fn begin(&mut self, retry: bool) -> Vec<BatchJob> {
        if self.running {
            return Vec::new();
        }
        let jobs = self
            .rows
            .iter_mut()
            .filter(|row| !retry || matches!(row.status, FileStatus::Failed(_)))
            .map(|row| {
                self.previous.push((row.job.id, row.status.clone()));
                row.status = FileStatus::Pending;
                row.job.clone()
            })
            .collect::<Vec<_>>();
        self.running = !jobs.is_empty();
        jobs
    }

    pub fn update(&mut self, update: BatchUpdate) -> Option<usize> {
        let index = *self.indices.get(&update.id)?;
        let row = self.rows.get_mut(index)?;
        row.status = update.status;
        Some(index)
    }

    pub fn finish(&mut self) {
        self.running = false;
        self.previous.clear();
        for row in &mut self.rows {
            if row.status == FileStatus::Processing {
                row.status = FileStatus::Pending;
            }
        }
    }

    /// Admission or destination validation failed before any file was dispatched.
    pub fn reject(&mut self) {
        self.running = false;
        for (id, status) in self.previous.drain(..) {
            if let Some(row) = self
                .indices
                .get(&id)
                .and_then(|index| self.rows.get_mut(*index))
            {
                row.status = status;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deleting_single_and_multiple_rows_preserves_files_and_allows_reimport() {
        let root = tempfile::tempdir().unwrap();
        let mut queue = Queue::default();
        let paths: Vec<_> = (0..5)
            .map(|index| root.path().join(format!("{index}.wav")))
            .collect();
        for path in &paths {
            std::fs::write(path, "audio").unwrap();
            queue.add(path.clone());
        }
        let output = root.path().join("0.txt");
        std::fs::write(&output, "completed transcript").unwrap();
        let first_id = queue.rows[0].job.id;
        queue.update(BatchUpdate {
            id: first_id,
            status: FileStatus::Completed {
                output: Some(output.clone()),
            },
        });
        let failed_id = queue.rows[2].job.id;
        queue.update(BatchUpdate {
            id: failed_id,
            status: FileStatus::Failed("failed".into()),
        });
        assert!(queue.can_retry());
        assert_eq!(queue.remove(&[2]), vec![failed_id]);
        assert!(!queue.can_retry());
        let ids: Vec<_> = queue.rows.iter().map(|row| row.job.id).collect();
        assert_eq!(queue.remove(&[3, 0, 3, usize::MAX]), vec![ids[0], ids[3]]);
        assert_eq!(
            queue
                .rows
                .iter()
                .map(|row| row.job.path.clone())
                .collect::<Vec<_>>(),
            vec![paths[1].clone(), paths[3].clone()]
        );
        assert!(queue.add(paths[0].clone()));
        assert!(queue.rows.last().unwrap().job.id > first_id);
        assert!(
            queue
                .update(BatchUpdate {
                    id: first_id,
                    status: FileStatus::Failed("late update".into())
                })
                .is_none()
        );
        assert!(paths.iter().all(|path| path.exists()));
        assert_eq!(
            std::fs::read_to_string(output).unwrap(),
            "completed transcript"
        );
        queue.remove(&[0, 1, 2]);
        assert!(queue.rows.is_empty());
    }

    #[test]
    fn removal_during_a_run_keeps_surviving_ids_and_rejection_does_not_restore_removed_rows() {
        let mut queue = Queue::default();
        for path in ["a.wav", "b.wav", "c.wav"] {
            queue.add(path.into());
        }
        let jobs = queue.begin(false);
        assert_eq!(queue.remove(&[0, 2]), vec![jobs[0].id, jobs[2].id]);
        assert!(queue.running);
        assert_eq!(
            queue.update(BatchUpdate {
                id: jobs[1].id,
                status: FileStatus::Completed {
                    output: Some("b.txt".into())
                }
            }),
            Some(0)
        );
        assert!(queue.add("a.wav".into()));
        let new_id = queue.rows[1].job.id;
        assert_ne!(new_id, jobs[0].id);
        assert!(
            queue
                .update(BatchUpdate {
                    id: jobs[0].id,
                    status: FileStatus::Completed {
                        output: Some("old-a.txt".into())
                    }
                })
                .is_none()
        );
        assert_eq!(queue.rows[1].status, FileStatus::Pending);
        queue.reject();
        assert_eq!(queue.rows.len(), 2);
        assert_eq!(queue.rows[0].job.id, jobs[1].id);
        assert_eq!(queue.rows[1].job.id, new_id);
    }
    #[test]
    fn retry_is_disabled_through_mixed_batch_and_only_resets_failed_rows() {
        let mut queue = Queue::default();
        assert!(queue.add("a.wav".into()));
        assert!(!queue.add("a.wav".into()));
        queue.add("b.wav".into());
        queue.add("c.wav".into());
        let jobs = queue.begin(false);
        assert_eq!(jobs.len(), 3);
        queue.update(BatchUpdate {
            id: jobs[0].id,
            status: FileStatus::Completed {
                output: Some("a.txt".into()),
            },
        });
        queue.update(BatchUpdate {
            id: jobs[1].id,
            status: FileStatus::Failed("failed".into()),
        });
        queue.update(BatchUpdate {
            id: jobs[2].id,
            status: FileStatus::Processing,
        });
        assert!(!queue.can_retry());
        queue.add("next.wav".into());
        queue.finish();
        assert!(queue.can_retry());
        assert_eq!(queue.rows[2].status, FileStatus::Pending);
        let retry = queue.begin(true);
        assert_eq!(retry.len(), 1);
        assert_eq!(retry[0].id, jobs[1].id);
        assert!(matches!(queue.rows[0].status, FileStatus::Completed { .. }));
        assert!(!queue.can_retry());
    }

    #[test]
    fn rejected_retry_preserves_failures_and_files_added_during_admission() {
        let mut queue = Queue::default();
        queue.add("a.wav".into());
        let id = queue.rows[0].job.id;
        queue.update(BatchUpdate {
            id,
            status: FileStatus::Failed("previous failure".into()),
        });
        assert_eq!(queue.begin(true).len(), 1);
        queue.add("new.wav".into());
        queue.reject();
        assert!(queue.can_retry());
        assert_eq!(
            queue.rows[0].status,
            FileStatus::Failed("previous failure".into())
        );
        assert_eq!(queue.rows[1].status, FileStatus::Pending);
    }
}
