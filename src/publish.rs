use std::collections::VecDeque;

use thiserror::Error;

use crate::model::{Platform, PublishJob};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PublishError {
    #[error("select at least one destination")]
    NoDestination,
    #[error("{0} does not support {1} visibility")]
    UnsupportedVisibility(&'static str, &'static str),
}

#[derive(Default)]
pub struct PublishQueue {
    jobs: VecDeque<PublishJob>,
}

impl PublishQueue {
    pub fn enqueue(&mut self, job: PublishJob) -> Result<(), PublishError> {
        let enabled: Vec<_> = job.targets.iter().filter(|target| target.enabled).collect();
        if enabled.is_empty() {
            return Err(PublishError::NoDestination);
        }
        for target in enabled {
            if !target
                .platform
                .supported_visibilities()
                .contains(&target.visibility)
            {
                return Err(PublishError::UnsupportedVisibility(
                    target.platform.label(),
                    target.visibility.label(),
                ));
            }
        }
        self.jobs.push_back(job);
        Ok(())
    }

    pub fn jobs(&self) -> impl Iterator<Item = &PublishJob> {
        self.jobs.iter()
    }

    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }
}

pub fn connection_help(platform: Platform) -> &'static str {
    match platform {
        Platform::YouTube => {
            "Requires Google OAuth with the youtube.upload scope. Public uploads require an audited API project."
        }
        Platform::Odysee => {
            "Requires an Odysee account/publish integration. Public and link-accessible Unlisted are supported; Private is not."
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use uuid::Uuid;

    use super::*;
    use crate::model::{PublishStatus, PublishTarget, Visibility};

    fn job(targets: Vec<PublishTarget>) -> PublishJob {
        PublishJob {
            id: Uuid::new_v4(),
            clip_id: Uuid::new_v4(),
            title: "test".into(),
            description: String::new(),
            targets,
            created_at: Utc::now(),
            status: PublishStatus::Queued,
        }
    }

    #[test]
    fn youtube_accepts_unlisted() {
        let mut queue = PublishQueue::default();
        let mut target = PublishTarget::new(Platform::YouTube);
        target.enabled = true;
        target.visibility = Visibility::Unlisted;
        assert!(queue.enqueue(job(vec![target])).is_ok());
    }

    #[test]
    fn odysee_rejects_false_privacy_promise() {
        let mut queue = PublishQueue::default();
        let mut target = PublishTarget::new(Platform::Odysee);
        target.enabled = true;
        target.visibility = Visibility::Private;
        assert_eq!(
            queue.enqueue(job(vec![target])),
            Err(PublishError::UnsupportedVisibility("Odysee", "Private"))
        );
    }

    #[test]
    fn odysee_accepts_unlisted() {
        let mut queue = PublishQueue::default();
        let mut target = PublishTarget::new(Platform::Odysee);
        target.enabled = true;
        target.visibility = Visibility::Unlisted;
        assert!(queue.enqueue(job(vec![target])).is_ok());
    }
}
