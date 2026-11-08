use super::{
    Arc, HarnessError, LocalApplication, LocalSession, Profile, resolve_named_provider,
    session_profile,
};

impl LocalApplication {
    /// Read the accepted configuration without contacting or authorizing a model source.
    /// Task preparation refreshes Provider web settings before execution.
    pub async fn effective_session_profile(
        &self,
        session_id: &str,
    ) -> Result<Profile, HarnessError> {
        let mut session = self
            .state
            .session(session_id)
            .await
            .ok_or_else(|| HarnessError::invalid(format!("unknown session {session_id:?}")))?;
        if let Some(snapshot) = &mut session.server_model
            && let Some(managed) = self.live.read().await.get(session_id)
        {
            snapshot.web_access.clone_from(&managed.web_access);
        }
        self.profile_for_record(&session).await
    }

    pub(super) async fn projection_units(
        &self,
        session: &LocalSession,
    ) -> Result<Vec<Arc<dyn ternilo_kernel::SessionProjectionUnit>>, HarnessError> {
        let profile = self.profile_for_record(session).await?;
        self.catalog.projection_units(&profile)
    }

    async fn profile_for_record(&self, session: &LocalSession) -> Result<Profile, HarnessError> {
        let named_provider = resolve_named_provider(&session.model, &self.providers).await?;
        Ok(session_profile(
            &self.profile,
            &session.model,
            session.server_model.as_ref(),
            named_provider.as_ref(),
            &session.preset_plugins,
            &session.profile_plugins,
            session.mode,
        ))
    }
}
