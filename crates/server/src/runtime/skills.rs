use super::ServerRuntime;
use crate::{
    ProtocolErrorCode, SkillChangedParams, SkillChangedResult, SkillListParams, SkillListResult,
    SuccessResponse,
};

impl ServerRuntime {
    pub(super) async fn handle_skills_list(
        &self,
        request_id: serde_json::Value,
        params: serde_json::Value,
    ) -> serde_json::Value {
        let params = match serde_json::from_value::<SkillListParams>(params) {
            Ok(params) => params,
            Err(error) => {
                return self.error_response(
                    request_id,
                    ProtocolErrorCode::InvalidParams,
                    format!("invalid skills/list params: {error}"),
                );
            }
        };

        match self.deps.discover_skills(params.cwd.as_deref()) {
            Ok(skills) => serde_json::to_value(SuccessResponse {
                id: request_id,
                result: SkillListResult { skills },
            })
            .expect("serialize skills/list response"),
            Err(error) => self.error_response(
                request_id,
                ProtocolErrorCode::InternalError,
                format!("failed to discover skills: {error}"),
            ),
        }
    }

    pub(super) async fn handle_skills_changed(
        &self,
        request_id: serde_json::Value,
        params: serde_json::Value,
    ) -> serde_json::Value {
        let params = match serde_json::from_value::<SkillChangedParams>(params) {
            Ok(params) => params,
            Err(error) => {
                return self.error_response(
                    request_id,
                    ProtocolErrorCode::InvalidParams,
                    format!("invalid skills/changed params: {error}"),
                );
            }
        };

        match self.deps.discover_skills(params.cwd.as_deref()) {
            Ok(skills) => serde_json::to_value(SuccessResponse {
                id: request_id,
                result: SkillChangedResult { skills },
            })
            .expect("serialize skills/changed response"),
            Err(error) => self.error_response(
                request_id,
                ProtocolErrorCode::InternalError,
                format!("failed to discover skills: {error}"),
            ),
        }
    }
}
