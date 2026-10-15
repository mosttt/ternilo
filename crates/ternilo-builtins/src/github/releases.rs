use super::{GitHub, HarnessError, Value, endpoint, first_page, json};
use reqwest::Method;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ListReleases {
    repository: String,
    #[serde(default = "first_page")]
    page: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReleaseStatus {
    repository: String,
    tag: Option<String>,
    release_id: Option<u64>,
    #[serde(default = "first_page")]
    assets_page: u32,
}

impl ReleaseStatus {
    fn url(&self, base: &str) -> Result<reqwest::Url, HarnessError> {
        if self.assets_page == 0 || self.release_id == Some(0) {
            return Err(HarnessError::invalid(
                "release ID and asset page must be positive",
            ));
        }
        match (&self.tag, self.release_id) {
            (Some(_), Some(_)) => Err(HarnessError::invalid(
                "select either tag or release_id, not both",
            )),
            (Some(tag), None) => {
                if tag.trim().is_empty()
                    || matches!(tag.as_str(), "." | "..")
                    || tag.len() > 256
                    || tag.chars().any(char::is_control)
                {
                    return Err(HarnessError::invalid(
                        "release tag must be a nonempty tag of at most 256 bytes",
                    ));
                }
                endpoint(base, &self.repository, &["releases", "tags", tag])
            }
            (None, Some(id)) => endpoint(base, &self.repository, &["releases", &id.to_string()]),
            (None, None) => endpoint(base, &self.repository, &["releases", "latest"]),
        }
    }
}

fn release(value: &Value) -> Value {
    json!({"id":value["id"],"url":value["html_url"],"name":value["name"],"tag":value["tag_name"],"target_commitish":value["target_commitish"],"body":value["body"],"draft":value["draft"],"prerelease":value["prerelease"],"immutable":value["immutable"],"created_at":value["created_at"],"published_at":value["published_at"]})
}

impl GitHub {
    pub(super) async fn list_releases(
        &self,
        args: ListReleases,
        key: Option<&str>,
    ) -> Result<Value, HarnessError> {
        if args.page == 0 {
            return Err(HarnessError::invalid("release page must be positive"));
        }
        let mut url = endpoint(&self.config.base_url, &args.repository, &["releases"])?;
        url.query_pairs_mut()
            .append_pair("per_page", "30")
            .append_pair("page", &args.page.to_string());
        let response = self.request(Method::GET, url, None, key).await?;
        let values = response
            .value
            .as_array()
            .ok_or_else(|| HarnessError::execution("GitHub release list is not an array"))?;
        Ok(
            json!({"releases":values.iter().map(release).collect::<Vec<_>>(),"page":args.page,"has_more":response.more}),
        )
    }

    pub(super) async fn release_status(
        &self,
        args: ReleaseStatus,
        key: Option<&str>,
    ) -> Result<Value, HarnessError> {
        let summary = self
            .request(Method::GET, args.url(&self.config.base_url)?, None, key)
            .await?
            .value;
        let id = summary["id"]
            .as_u64()
            .filter(|id| *id > 0)
            .ok_or_else(|| HarnessError::execution("GitHub release is missing its ID"))?;
        let mut url = endpoint(
            &self.config.base_url,
            &args.repository,
            &["releases", &id.to_string(), "assets"],
        )?;
        url.query_pairs_mut()
            .append_pair("per_page", "100")
            .append_pair("page", &args.assets_page.to_string());
        let assets = self.request(Method::GET, url, None, key).await?;
        let values = assets
            .value
            .as_array()
            .ok_or_else(|| HarnessError::execution("GitHub release assets are not an array"))?;
        Ok(json!({
            "release":release(&summary),
            "assets":values.iter().map(|asset| json!({"id":asset["id"],"name":asset["name"],"label":asset["label"],"state":asset["state"],"size":asset["size"],"content_type":asset["content_type"],"download_url":asset["browser_download_url"],"download_count":asset["download_count"],"digest":asset["digest"],"created_at":asset["created_at"],"updated_at":asset["updated_at"]})).collect::<Vec<_>>(),
            "assets_page":args.assets_page,"has_more_assets":assets.more,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn release_selection_is_explicit_and_tags_are_one_path_segment() {
        let make = |extra: Value| {
            let mut args = json!({"repository":"owner/repo"});
            args.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            serde_json::from_value::<ReleaseStatus>(args)
                .unwrap()
                .url("https://example.invalid/api/v3")
        };
        assert!(
            make(json!({}))
                .unwrap()
                .path()
                .ends_with("/releases/latest")
        );
        assert!(
            make(json!({"release_id":17}))
                .unwrap()
                .path()
                .ends_with("/releases/17")
        );
        assert!(
            make(json!({"tag":"series/v1"}))
                .unwrap()
                .path()
                .ends_with("/tags/series%2Fv1")
        );
        for extra in [
            json!({"tag":"v1","release_id":17}),
            json!({"tag":".."}),
            json!({"release_id":0}),
            json!({"assets_page":0}),
        ] {
            assert!(make(extra).is_err());
        }
    }
}
