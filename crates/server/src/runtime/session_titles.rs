use std::sync::Arc;

use chrono::Utc;

use lpa_core::{SessionId, SessionTitleFinalSource, SessionTitleState};

use crate::{
    ServerEvent, SessionEventPayload,
    titles::{build_title_generation_request, derive_provisional_title, normalize_generated_title},
};

use super::ServerRuntime;

impl ServerRuntime {
    pub(super) async fn maybe_assign_provisional_title(
        &self,
        session_id: SessionId,
        first_user_input: &str,
    ) {
        let Some(candidate) = derive_provisional_title(first_user_input) else {
            return;
        };
        let Some(session_arc) = self.sessions.lock().await.get(&session_id).cloned() else {
            return;
        };

        let updated_summary = {
            let mut session = session_arc.lock().await;
            if session.summary.title.is_some()
                || !matches!(session.summary.title_state, SessionTitleState::Unset)
            {
                return;
            }

            let previous_title = session.summary.title.clone();
            let updated_at = Utc::now();
            session.summary.title = Some(candidate.clone());
            session.summary.title_state = SessionTitleState::Provisional;
            session.summary.updated_at = updated_at;

            if let Some(record) = session.record.as_mut() {
                record.title = Some(candidate.clone());
                record.title_state = SessionTitleState::Provisional;
                record.updated_at = updated_at;
                if let Err(error) = self.rollout_store.append_title_update(
                    record,
                    candidate.clone(),
                    SessionTitleState::Provisional,
                    previous_title,
                ) {
                    tracing::warn!(session_id = %session_id, error = %error, "failed to persist provisional title");
                }
            }
            session.summary.clone()
        };

        self.broadcast_event(ServerEvent::SessionTitleUpdated(SessionEventPayload {
            session: updated_summary,
        }))
        .await;
    }

    pub(super) async fn maybe_generate_final_title(
        self: Arc<Self>,
        session_id: SessionId,
        first_user_input: &str,
        first_assistant_reply: &str,
    ) {
        let (model, title_state) = {
            let Some(session_arc) = self.sessions.lock().await.get(&session_id).cloned() else {
                return;
            };
            let session = session_arc.lock().await;
            (
                session
                    .summary
                    .resolved_model
                    .clone()
                    .unwrap_or_else(|| self.deps.default_model.clone()),
                session.summary.title_state.clone(),
            )
        };

        if matches!(
            title_state,
            SessionTitleState::Final(SessionTitleFinalSource::ExplicitCreate)
                | SessionTitleState::Final(SessionTitleFinalSource::UserRename)
                | SessionTitleState::Final(SessionTitleFinalSource::ModelGenerated)
        ) {
            return;
        }

        let response = match self
            .deps
            .provider
            .completion(build_title_generation_request(
                model,
                first_user_input,
                first_assistant_reply,
            ))
            .await
        {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(session_id = %session_id, error = %error, "title generation request failed");
                return;
            }
        };
        let Some(generated_title) = normalize_generated_title(&response.content) else {
            tracing::warn!(session_id = %session_id, "title generation returned no valid title");
            return;
        };

        let Some(session_arc) = self.sessions.lock().await.get(&session_id).cloned() else {
            return;
        };
        let updated_summary = {
            let mut session = session_arc.lock().await;
            if matches!(
                session.summary.title_state,
                SessionTitleState::Final(SessionTitleFinalSource::ExplicitCreate)
                    | SessionTitleState::Final(SessionTitleFinalSource::UserRename)
                    | SessionTitleState::Final(SessionTitleFinalSource::ModelGenerated)
            ) {
                return;
            }

            let previous_title = session.summary.title.clone();
            let updated_at = Utc::now();
            session.summary.title = Some(generated_title.clone());
            session.summary.title_state =
                SessionTitleState::Final(SessionTitleFinalSource::ModelGenerated);
            session.summary.updated_at = updated_at;

            if let Some(record) = session.record.as_mut() {
                record.title = Some(generated_title.clone());
                record.title_state =
                    SessionTitleState::Final(SessionTitleFinalSource::ModelGenerated);
                record.updated_at = updated_at;
                if let Err(error) = self.rollout_store.append_title_update(
                    record,
                    generated_title.clone(),
                    record.title_state.clone(),
                    previous_title,
                ) {
                    tracing::warn!(session_id = %session_id, error = %error, "failed to persist generated title");
                }
            }
            session.summary.clone()
        };

        self.broadcast_event(ServerEvent::SessionTitleUpdated(SessionEventPayload {
            session: updated_summary,
        }))
        .await;
    }
}
