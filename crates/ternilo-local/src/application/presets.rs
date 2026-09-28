use super::{
    AgentPresetCopyRequest, AgentPresetDocument, AgentPresetRoster, AgentPresetUpdateRequest,
    HarnessError, LocalApplication, compose_profiles, now_ms, validate_local_profile,
};

impl LocalApplication {
    pub async fn agent_preset_roster(&self) -> AgentPresetRoster {
        self.presets.roster().await
    }

    pub async fn agent_preset(&self, id: &str) -> Result<AgentPresetDocument, HarnessError> {
        self.presets
            .resolve(id)
            .await
            .ok_or_else(|| HarnessError::invalid(format!("unknown agent preset {id:?}")))
    }

    /// Editor views inherit from this host's base, never from a selected session.
    pub async fn agent_preset_view(&self, id: &str) -> Result<AgentPresetDocument, HarnessError> {
        let mut preset = self.agent_preset(id).await?;
        preset.base_profile = Some(self.profile.clone());
        Ok(preset)
    }

    pub async fn copy_agent_preset(
        &self,
        request: AgentPresetCopyRequest,
    ) -> Result<AgentPresetDocument, HarnessError> {
        let preset = self.presets.copy(request).await?;
        self.invalidate(None, crate::LocalInvalidationCategory::Workbench, None);
        Ok(preset)
    }

    pub async fn update_agent_preset(
        &self,
        id: &str,
        request: AgentPresetUpdateRequest,
    ) -> Result<AgentPresetDocument, HarnessError> {
        validate_local_profile(
            &compose_profiles([self.profile.clone(), request.profile.clone()]),
            &self.catalog,
            &self.extension_registry,
        )?;
        let preset = self.presets.update(id, request).await?;
        self.invalidate(None, crate::LocalInvalidationCategory::Workbench, None);
        Ok(preset)
    }

    pub async fn remove_agent_preset(&self, id: &str) -> Result<(), HarnessError> {
        self.presets.remove(id).await?;
        self.invalidate(None, crate::LocalInvalidationCategory::Workbench, None);
        Ok(())
    }

    pub async fn set_default_agent_preset(
        &self,
        id: &str,
    ) -> Result<AgentPresetRoster, HarnessError> {
        let roster = self.presets.set_default(id).await?;
        self.invalidate(None, crate::LocalInvalidationCategory::Workbench, None);
        Ok(roster)
    }

    pub fn extension_inventory(
        &self,
    ) -> Result<ternilo_extension::ExtensionInventory, HarnessError> {
        self.extension_registry.inventory()
    }

    pub fn trust_extension_publisher(
        &self,
        trust: ternilo_extension::PublisherTrust,
    ) -> Result<ternilo_extension::TrustedPublisher, HarnessError> {
        self.extension_registry.trust_publisher(trust, now_ms()?)
    }

    pub async fn revoke_extension_publisher(&self, key_id: &str) -> Result<(), HarnessError> {
        let packages = self
            .extension_registry
            .inventory()?
            .extensions
            .into_iter()
            .filter(|extension| extension.manifest.publisher_key_id == key_id)
            .map(|extension| (extension.manifest.package_id, extension.manifest.version))
            .collect::<Vec<_>>();
        self.remove_extension_mounts(&packages).await?;
        self.extension_registry.revoke_publisher(key_id, now_ms()?)
    }

    pub fn install_extension(
        &self,
        request: ternilo_extension::ExtensionInstallRequest,
    ) -> Result<ternilo_extension::InstalledExtension, HarnessError> {
        self.extension_registry.install(request, now_ms()?)
    }

    pub async fn set_extension_enabled(
        &self,
        package_id: &str,
        version: &str,
        enabled: bool,
    ) -> Result<ternilo_extension::InstalledExtension, HarnessError> {
        self.extension_registry.describe(package_id, version)?;
        if !enabled {
            self.remove_extension_mounts(&[(package_id.to_owned(), version.to_owned())])
                .await?;
        }
        self.extension_registry
            .set_enabled(package_id, version, enabled, now_ms()?)
    }

    pub async fn set_extension_enabled_for_web(
        &self,
        package_id: &str,
        version: &str,
        enabled: bool,
    ) -> Result<ternilo_extension::InstalledExtension, HarnessError> {
        self.set_extension_enabled(package_id, version, enabled)
            .await
    }

    pub async fn revoke_extension(
        &self,
        package_id: &str,
        version: &str,
    ) -> Result<(), HarnessError> {
        self.extension_registry.describe(package_id, version)?;
        self.remove_extension_mounts(&[(package_id.to_owned(), version.to_owned())])
            .await?;
        self.extension_registry
            .revoke(package_id, version, now_ms()?)
    }

    pub async fn uninstall_extension(
        &self,
        package_id: &str,
        version: &str,
    ) -> Result<(), HarnessError> {
        self.extension_registry.describe(package_id, version)?;
        self.remove_extension_mounts(&[(package_id.to_owned(), version.to_owned())])
            .await?;
        self.extension_registry.uninstall(package_id, version)
    }

    pub(super) async fn remove_extension_mounts(
        &self,
        packages: &[(String, String)],
    ) -> Result<(), HarnessError> {
        let mut affected = self
            .state
            .snapshot()
            .await
            .sessions
            .into_iter()
            .filter_map(
                |mut session| match crate::extensions::remove_extension_mounts_from_session(
                    &self.profile,
                    &mut session.preset_plugins,
                    &mut session.profile_plugins,
                    packages,
                ) {
                    Ok(true) => Some(Ok(session)),
                    Ok(false) => None,
                    Err(error) => Some(Err(error)),
                },
            )
            .collect::<Result<Vec<_>, _>>()?;
        for session in &affected {
            self.stop_live_session(session.identity.session_id.as_str())
                .await?;
        }
        for session in &mut affected {
            session.updated_at_ms = now_ms()?;
            self.state
                .replace_session(session.identity.session_id.as_str(), session.clone())
                .await?;
            self.invalidate(
                Some(session.identity.session_id.as_str()),
                crate::LocalInvalidationCategory::Profile,
                None,
            );
            self.invalidate(
                Some(session.identity.session_id.as_str()),
                crate::LocalInvalidationCategory::Workbench,
                None,
            );
        }
        self.presets.remove_extension_mounts(packages).await
    }
}
