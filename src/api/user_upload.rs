use chrono::{DateTime, Utc};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use regex::Regex;
use serde::Deserialize;
use std::sync::LazyLock;

use crate::api::{API_BASE_URL, ApiClient, ApiError, check_for_response_success};

/// Regex for validating user_id contains only safe characters (alphanumeric, hyphen, underscore)
/// This prevents injection attacks and ensures the ID can be safely used in URLs
static USER_ID_REGEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9_-]+$").unwrap());

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct UserUploads {
    pub statistics: UserUploadStatistics,
    pub uploads: Vec<UserUpload>,
    pub limit: u32,
    pub offset: u32,
}

#[derive(Deserialize, Debug, Clone)]
#[allow(unused)]
pub struct UserUploadStatistics {
    pub total_uploads: u64,
    pub total_data: UserUploadDataSize,
    pub total_video_time: UserUploadVideoTime,
    pub verified_uploads: u32,
}

#[derive(Deserialize, Debug, Clone)]
#[allow(unused)]
pub struct UserUploadDataSize {
    pub bytes: u64,
    pub megabytes: f64,
    pub gigabytes: f64,
}

#[derive(Deserialize, Debug, Clone)]
#[allow(unused)]
pub struct UserUploadVideoTime {
    pub seconds: f64,
    pub minutes: f64,
    pub hours: f64,
    pub formatted: String,
}

/// this struct has to be public for config defining UploadStats to reference
#[derive(Deserialize, Debug, Clone)]
#[allow(unused)]
pub struct UserUpload {
    pub content_type: String,
    pub created_at: DateTime<Utc>,
    pub file_size_bytes: u64,
    pub file_size_mb: f64,
    pub filename: String,
    pub id: String,
    pub tags: Option<serde_json::Value>,
    pub verified: bool,
    pub video_duration_seconds: Option<f64>,
}

impl ApiClient {
    const MAX_USER_ID_LENGTH: usize = 256;
    /// Maximum number of uploads to request in a single API call (pagination limit)
    const MAX_UPLOADS_LIMIT: u32 = 1000;
    /// Maximum offset for pagination to prevent resource exhaustion
    const MAX_UPLOADS_OFFSET: u32 = 10_000_000;

    fn validate_user_id(user_id: &str) -> Result<(), ApiError> {
        if user_id.is_empty() {
            return Err(ApiError::ApiKeyValidationFailure(
                "User ID cannot be empty".into(),
            ));
        }
        if user_id.len() > Self::MAX_USER_ID_LENGTH {
            return Err(ApiError::ApiKeyValidationFailure(
                "User ID exceeds maximum length".into(),
            ));
        }
        // Validate user_id contains only safe alphanumeric characters, hyphens, and underscores
        // This prevents injection attacks and ensures proper URL encoding behavior
        if !USER_ID_REGEX.is_match(user_id) {
            return Err(ApiError::ApiKeyValidationFailure(
                "User ID contains invalid characters (allowed: alphanumeric, hyphen, underscore)"
                    .into(),
            ));
        }
        Ok(())
    }

    pub async fn get_user_upload_statistics(
        &self,
        api_key: &str,
        user_id: &str,
        start_date: Option<chrono::NaiveDate>,
        end_date: Option<chrono::NaiveDate>,
    ) -> Result<UserUploadStatistics, ApiError> {
        #[derive(Deserialize, Debug)]
        #[allow(unused)]
        struct UserStatisticsResponse {
            success: bool,
            user_id: String,
            statistics: UserUploadStatistics,
        }

        Self::validate_user_id(user_id)?;
        let encoded_user_id = utf8_percent_encode(user_id, NON_ALPHANUMERIC).to_string();
        let mut url = format!(
            "{}/tracker/v2/uploads/user/{encoded_user_id}/stats",
            API_BASE_URL.as_str()
        );
        let mut query_params = Vec::new();
        if let Some(start) = start_date {
            let date_str = start.format("%Y-%m-%d").to_string();
            let encoded_date = utf8_percent_encode(&date_str, NON_ALPHANUMERIC).to_string();
            query_params.push(format!("start_date={}", encoded_date));
        }
        if let Some(end) = end_date {
            let date_str = end.format("%Y-%m-%d").to_string();
            let encoded_date = utf8_percent_encode(&date_str, NON_ALPHANUMERIC).to_string();
            query_params.push(format!("end_date={}", encoded_date));
        }
        if !query_params.is_empty() {
            url.push('?');
            url.push_str(&query_params.join("&"));
        }

        let response = self
            .client
            .get(url)
            .header("Content-Type", "application/json")
            .header("X-API-Key", api_key)
            .send()
            .await?;

        let response =
            check_for_response_success(response, "User upload statistics unavailable").await?;

        let server_stats = response.json::<UserStatisticsResponse>().await?;

        if !server_stats.success {
            return Err(ApiError::ApiFailure {
                context: "User upload statistics unavailable".into(),
                error: "Server reported failure".into(),
                status: None,
            });
        }

        Ok(server_stats.statistics)
    }

    pub async fn get_user_upload_list(
        &self,
        api_key: &str,
        user_id: &str,
        limit: u32,
        offset: u32,
        start_date: Option<chrono::NaiveDate>,
        end_date: Option<chrono::NaiveDate>,
    ) -> Result<(Vec<UserUpload>, u32, u32), ApiError> {
        #[derive(Deserialize, Debug)]
        #[allow(unused)]
        struct UserUploadListResponse {
            success: bool,
            user_id: String,
            uploads: Vec<UserUpload>,
            limit: u32,
            offset: u32,
        }

        Self::validate_user_id(user_id)?;

        // Validate limit to prevent DoS from requesting too many records
        if limit == 0 || limit > Self::MAX_UPLOADS_LIMIT {
            return Err(ApiError::ApiKeyValidationFailure(
                format!("Limit must be between 1 and {}", Self::MAX_UPLOADS_LIMIT).into(),
            ));
        }
        // Validate offset to prevent resource exhaustion from deep pagination
        if offset > Self::MAX_UPLOADS_OFFSET {
            return Err(ApiError::ApiKeyValidationFailure(
                format!("Offset cannot exceed {}", Self::MAX_UPLOADS_OFFSET).into(),
            ));
        }

        let encoded_user_id = utf8_percent_encode(user_id, NON_ALPHANUMERIC).to_string();
        let mut url = format!(
            "{}/tracker/v2/uploads/user/{encoded_user_id}/list?limit={limit}&offset={offset}",
            API_BASE_URL.as_str()
        );
        if let Some(start) = start_date {
            let date_str = start.format("%Y-%m-%d").to_string();
            let encoded_date = utf8_percent_encode(&date_str, NON_ALPHANUMERIC).to_string();
            url.push_str(&format!("&start_date={}", encoded_date));
        }
        if let Some(end) = end_date {
            let date_str = end.format("%Y-%m-%d").to_string();
            let encoded_date = utf8_percent_encode(&date_str, NON_ALPHANUMERIC).to_string();
            url.push_str(&format!("&end_date={}", encoded_date));
        }

        let response = self
            .client
            .get(url)
            .header("Content-Type", "application/json")
            .header("X-API-Key", api_key)
            .send()
            .await?;

        let response = check_for_response_success(response, "User upload list unavailable").await?;

        let server_list = response.json::<UserUploadListResponse>().await?;

        if !server_list.success {
            return Err(ApiError::ApiFailure {
                context: "User upload list unavailable".into(),
                error: "Server reported failure".into(),
                status: None,
            });
        }

        Ok((server_list.uploads, server_list.limit, server_list.offset))
    }

    /// Legacy method for backward compatibility if needed, though it's better to use the split methods.
    #[allow(dead_code)]
    pub async fn get_user_upload_stats(
        &self,
        api_key: &str,
        user_id: &str,
        limit: u32,
        offset: u32,
    ) -> Result<UserUploads, ApiError> {
        Self::validate_user_id(user_id)?;

        // Validate limit to prevent DoS from requesting too many records
        if limit == 0 || limit > Self::MAX_UPLOADS_LIMIT {
            return Err(ApiError::ApiKeyValidationFailure(
                format!("Limit must be between 1 and {}", Self::MAX_UPLOADS_LIMIT).into(),
            ));
        }
        // Validate offset to prevent resource exhaustion from deep pagination
        if offset > Self::MAX_UPLOADS_OFFSET {
            return Err(ApiError::ApiKeyValidationFailure(
                format!("Offset cannot exceed {}", Self::MAX_UPLOADS_OFFSET).into(),
            ));
        }

        let statistics = self
            .get_user_upload_statistics(api_key, user_id, None, None)
            .await?;
        let (uploads, limit, offset) = self
            .get_user_upload_list(api_key, user_id, limit, offset, None, None)
            .await?;

        Ok(UserUploads {
            statistics,
            uploads,
            limit,
            offset,
        })
    }
}
