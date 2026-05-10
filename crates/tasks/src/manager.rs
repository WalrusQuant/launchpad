use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::{TaskInfo, TaskNotification, TaskState};

/// Manages the lifecycle of background tasks.
///
/// The manager tracks all spawned tasks, collects their notifications,
/// and makes completed task output available for injection into the
/// conversation.
pub struct TaskManager {
    tasks: Arc<RwLock<HashMap<String, TaskInfo>>>,
    notifications: Arc<RwLock<Vec<TaskNotification>>>,
}

impl TaskManager {
    pub fn new() -> Self {
        Self {
            tasks: Arc::new(RwLock::new(HashMap::new())),
            notifications: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub async fn register(&self, info: TaskInfo) {
        info!(task_id = %info.id, name = %info.name, "task registered");
        self.tasks.write().await.insert(info.id.clone(), info);
    }

    pub async fn update_state(&self, task_id: &str, state: TaskState) {
        if let Some(info) = self.tasks.write().await.get_mut(task_id) {
            info.state = state;
            if matches!(
                state,
                TaskState::Completed | TaskState::Failed | TaskState::Cancelled
            ) {
                info.finished_at = Some(chrono::Utc::now());
            }
        }
    }

    pub async fn set_output(&self, task_id: &str, output: String) {
        if let Some(info) = self.tasks.write().await.get_mut(task_id) {
            info.output = Some(output);
        }
    }

    pub async fn push_notification(&self, notification: TaskNotification) {
        info!(task_id = %notification.task_id, "task notification");
        self.notifications.write().await.push(notification);
    }

    /// Drain all pending notifications for injection into the next turn.
    pub async fn drain_notifications(&self) -> Vec<TaskNotification> {
        let mut notifs = self.notifications.write().await;
        std::mem::take(&mut *notifs)
    }

    pub async fn get(&self, task_id: &str) -> Option<TaskInfo> {
        self.tasks.read().await.get(task_id).cloned()
    }

    pub async fn list(&self) -> Vec<TaskInfo> {
        self.tasks.read().await.values().cloned().collect()
    }

    pub async fn cancel(&self, task_id: &str) {
        warn!(task_id, "cancel requested");
        self.update_state(task_id, TaskState::Cancelled).await;
    }
}

impl Default for TaskManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TaskState;

    fn make_task_info(id: &str, name: &str) -> TaskInfo {
        TaskInfo {
            id: id.into(),
            name: name.into(),
            state: TaskState::Pending,
            output: None,
            created_at: chrono::Utc::now(),
            finished_at: None,
        }
    }

    #[tokio::test]
    async fn register_and_get() {
        let mgr = TaskManager::new();
        let info = make_task_info("t1", "compile");
        mgr.register(info).await;

        let task = mgr.get("t1").await;
        assert!(task.is_some());
        assert_eq!(task.unwrap().name, "compile");
    }

    #[tokio::test]
    async fn get_nonexistent_returns_none() {
        let mgr = TaskManager::new();
        assert!(mgr.get("nope").await.is_none());
    }

    #[tokio::test]
    async fn update_state_to_completed() {
        let mgr = TaskManager::new();
        mgr.register(make_task_info("t1", "build")).await;
        mgr.update_state("t1", TaskState::Running).await;

        let task = mgr.get("t1").await.unwrap();
        assert_eq!(task.state, TaskState::Running);
        assert!(task.finished_at.is_none());

        mgr.update_state("t1", TaskState::Completed).await;
        let task = mgr.get("t1").await.unwrap();
        assert_eq!(task.state, TaskState::Completed);
        assert!(task.finished_at.is_some());
    }

    #[tokio::test]
    async fn set_output() {
        let mgr = TaskManager::new();
        mgr.register(make_task_info("t1", "run")).await;
        mgr.set_output("t1", "success output".into()).await;

        let task = mgr.get("t1").await.unwrap();
        assert_eq!(task.output, Some("success output".into()));
    }

    #[tokio::test]
    async fn notifications_drain() {
        let mgr = TaskManager::new();
        mgr.push_notification(TaskNotification {
            task_id: "t1".into(),
            message: "step 1 done".into(),
            is_final: false,
        })
        .await;
        mgr.push_notification(TaskNotification {
            task_id: "t1".into(),
            message: "finished".into(),
            is_final: true,
        })
        .await;

        let notifs = mgr.drain_notifications().await;
        assert_eq!(notifs.len(), 2);
        assert_eq!(notifs[0].message, "step 1 done");
        assert!(notifs[1].is_final);

        // After drain, should be empty
        let notifs = mgr.drain_notifications().await;
        assert!(notifs.is_empty());
    }

    #[tokio::test]
    async fn list_all_tasks() {
        let mgr = TaskManager::new();
        mgr.register(make_task_info("t1", "a")).await;
        mgr.register(make_task_info("t2", "b")).await;
        mgr.register(make_task_info("t3", "c")).await;

        let tasks = mgr.list().await;
        assert_eq!(tasks.len(), 3);
    }

    #[tokio::test]
    async fn cancel_sets_state_and_finished_at() {
        let mgr = TaskManager::new();
        mgr.register(make_task_info("t1", "long_task")).await;
        mgr.cancel("t1").await;

        let task = mgr.get("t1").await.unwrap();
        assert_eq!(task.state, TaskState::Cancelled);
        assert!(task.finished_at.is_some());
    }

    #[tokio::test]
    async fn update_state_nonexistent_is_no_op() {
        let mgr = TaskManager::new();
        mgr.update_state("nonexistent", TaskState::Failed).await;
        assert!(mgr.get("nonexistent").await.is_none());
    }
}
