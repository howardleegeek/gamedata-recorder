use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde::{Deserialize, Serialize};

use crate::api::{API_BASE_URL, ApiClient, ApiError, check_for_response_success};

#[derive(Default, Debug, Clone, Deserialize, Serialize)]
#[allow(unused)]
pub struct InitMultipartUploadArgs<'a> {
    pub filename: &'a str,
    pub total_size_bytes: u64,
    pub hardware_id: &'a str,
    pub tags: Option<Vec<String>>,
    pub video_filename: Option<&'a str>,
    pub control_filename: Option<&'a str>,
    pub video_duration_seconds: Option<f64>,
    pub video_width: Option<u32>,
    pub video_height: Option<u32>,
    pub video_codec: Option<&'a str>,
    pub video_fps: Option<f32>,
    pub chunk_size_bytes: Option<u64>,
    /// Optional additional metadata. Use None to skip serialization entirely.
    pub additional_metadata: Option<serde_json::Value>,
    #[serde(alias = "uploading_owl_control_version")]
    pub uploading_recorder_version: Option<&'a str>,
}

#[derive(Deserialize, Debug)]
#[allow(unused)]
pub struct InitMultipartUploadResponse {
    pub upload_id: String,
    pub game_control_id: String,
    pub total_chunks: u64,
    pub chunk_size_bytes: u64,
    /// Unix timestamp
    pub expires_at: u64,
}

#[derive(Deserialize, Debug)]
#[allow(unused)]
pub struct UploadMultipartChunkResponse {
    pub upload_url: String,
    pub chunk_number: u64,
    /// Unix timestamp
    pub expires_at: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CompleteMultipartUploadChunk {
    pub chunk_number: u64,
    pub etag: String,
}

#[derive(Deserialize, Debug)]
#[allow(unused)]
pub struct CompleteMultipartUploadResponse {
    pub success: bool,
    pub game_control_id: String,
    pub object_key: String,
    pub message: String,
    #[serde(default)]
    pub verified: Option<bool>,
}

#[derive(Deserialize, Debug)]
#[allow(unused)]
pub struct AbortMultipartUploadResponse {
    pub success: bool,
    pub message: String,
}

impl ApiClient {
    const MAX_UPLOAD_ID_LENGTH: usize = 256;
    const MAX_FILENAME_LENGTH: usize = 1024;
    const MAX_HARDWARE_ID_LENGTH: usize = 256;

    fn validate_upload_id(upload_id: &str) -> Result<(), ApiError> {
        if upload_id.is_empty() {
            return Err(ApiError::ApiKeyValidationFailure(
                "Upload ID cannot be empty".into(),
            ));
        }
        if upload_id.len() > Self::MAX_UPLOAD_ID_LENGTH {
            return Err(ApiError::ApiKeyValidationFailure(
                "Upload ID exceeds maximum length".into(),
            ));
        }
        Ok(())
    }

    /// Validates a filename to prevent path traversal attacks and invalid names.
    fn validate_filename(filename: &str) -> Result<(), ApiError> {
        if filename.trim().is_empty() {
            return Err(ApiError::ApiKeyValidationFailure(
                "Filename cannot be empty or whitespace".into(),
            ));
        }
        if filename.len() > Self::MAX_FILENAME_LENGTH {
            return Err(ApiError::ApiKeyValidationFailure(
                format!(
                    "Filename exceeds maximum length of {} characters",
                    Self::MAX_FILENAME_LENGTH
                )
                .into(),
            ));
        }
        // Prevent path traversal attacks
        if filename.contains("..") || filename.contains('/') || filename.contains('\\') {
            return Err(ApiError::ApiKeyValidationFailure(
                "Filename contains invalid characters (path traversal)".into(),
            ));
        }
        Ok(())
    }

    /// Validates hardware ID to prevent empty or overly long values.
    fn validate_hardware_id(hardware_id: &str) -> Result<(), ApiError> {
        if hardware_id.is_empty() || hardware_id.trim().is_empty() {
            return Err(ApiError::ApiKeyValidationFailure(
                "Hardware ID cannot be empty or whitespace".into(),
            ));
        }
        if hardware_id.len() > Self::MAX_HARDWARE_ID_LENGTH {
            return Err(ApiError::ApiKeyValidationFailure(
                format!(
                    "Hardware ID exceeds maximum length of {} characters",
                    Self::MAX_HARDWARE_ID_LENGTH
                )
                .into(),
            ));
        }
        Ok(())
    }

    pub async fn init_multipart_upload<'a>(
        &self,
        api_key: &str,
        args: InitMultipartUploadArgs<'a>,
    ) -> Result<InitMultipartUploadResponse, ApiError> {
        #[derive(Serialize, Debug)]
        struct InitMultipartUploadRequest<'a> {
            filename: &'a str,
            content_type: &'a str,
            total_size_bytes: u64,
            #[serde(skip_serializing_if = "Option::is_none")]
            chunk_size_bytes: Option<u64>,

            #[serde(skip_serializing_if = "Option::is_none")]
            tags: Option<Vec<String>>,

            #[serde(skip_serializing_if = "Option::is_none")]
            video_filename: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            control_filename: Option<&'a str>,

            #[serde(skip_serializing_if = "Option::is_none")]
            video_duration_seconds: Option<f64>,
            #[serde(skip_serializing_if = "Option::is_none")]
            video_width: Option<u32>,
            #[serde(skip_serializing_if = "Option::is_none")]
            video_height: Option<u32>,
            #[serde(skip_serializing_if = "Option::is_none")]
            video_codec: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            video_fps: Option<f32>,

            #[serde(skip_serializing_if = "Option::is_none")]
            additional_metadata: Option<serde_json::Value>,

            #[serde(skip_serializing_if = "Option::is_none")]
            uploading_recorder_version: Option<&'a str>,

            uploader_hwid: &'a str,
            upload_timestamp: &'a str,
        }

        // Validate total_size_bytes is non-zero to prevent useless API calls
        if args.total_size_bytes == 0 {
            return Err(ApiError::ApiKeyValidationFailure(
                "Total size must be greater than 0".into(),
            ));
        }

        // Validate filename to prevent path traversal and invalid names
        Self::validate_filename(args.filename)?;

        // Validate hardware_id to prevent empty or invalid values
        Self::validate_hardware_id(args.hardware_id)?;

        // Validate video_duration_seconds if provided to prevent invalid data
        if let Some(duration) = args.video_duration_seconds {
            if duration.is_nan() || duration.is_infinite() || duration < 0.0 {
                return Err(ApiError::ApiKeyValidationFailure(
                    "Video duration must be a finite non-negative number".into(),
                ));
            }
        }

        // Store timestamp in a variable to prevent dangling reference
        let timestamp = chrono::Local::now().to_rfc3339();
        let response = self
            .client
            .post(format!(
                "{}/tracker/upload/game_control/multipart/init",
                API_BASE_URL.as_str()
            ))
            .header("Content-Type", "application/json")
            .header("X-API-Key", api_key)
            .json(&InitMultipartUploadRequest {
                filename: args.filename,
                content_type: "application/x-tar",
                total_size_bytes: args.total_size_bytes,
                chunk_size_bytes: args.chunk_size_bytes,

                tags: args.tags,

                video_filename: args.video_filename,
                control_filename: args.control_filename,

                video_duration_seconds: args.video_duration_seconds,
                video_width: args.video_width,
                video_height: args.video_height,
                video_codec: args.video_codec,
                video_fps: args.video_fps,

                additional_metadata: args.additional_metadata,

                uploading_recorder_version: args.uploading_recorder_version,

                uploader_hwid: args.hardware_id,
                upload_timestamp: &timestamp,
            })
            .send()
            .await?;

        let response = check_for_response_success(response, "Upload initialization failed")
            .await?
            .json::<InitMultipartUploadResponse>()
            .await?;

        // Validate expires_at to prevent integer overflow when cast to i64 in downstream code
        if response.expires_at > i64::MAX as u64 {
            return Err(ApiError::ServerInvalidation(format!(
                "Server returned expires_at too large: {} (max: {})",
                response.expires_at,
                i64::MAX as u64
            )));
        }

        // Validate total_chunks is non-zero to prevent downstream divide-by-zero issues
        if response.total_chunks == 0 {
            return Err(ApiError::ServerInvalidation(
                "Server returned invalid total_chunks: 0".into(),
            ));
        }

        // Validate chunk_size_bytes is reasonable to prevent downstream issues
        const MIN_CHUNK_SIZE: u64 = 1024; // 1KB minimum
        const MAX_CHUNK_SIZE: u64 = 512 * 1024 * 1024; // 512MB maximum
        if response.chunk_size_bytes == 0 {
            return Err(ApiError::ServerInvalidation(
                "Server returned invalid chunk_size_bytes: 0".into(),
            ));
        }
        if response.chunk_size_bytes < MIN_CHUNK_SIZE {
            return Err(ApiError::ServerInvalidation(format!(
                "Server returned chunk_size_bytes too small: {} (min: {})",
                response.chunk_size_bytes, MIN_CHUNK_SIZE
            )));
        }
        if response.chunk_size_bytes > MAX_CHUNK_SIZE {
            return Err(ApiError::ServerInvalidation(format!(
                "Server returned chunk_size_bytes too large: {} (max: {})",
                response.chunk_size_bytes, MAX_CHUNK_SIZE
            )));
        }

        Ok(response)
    }

    pub async fn upload_multipart_chunk(
        &self,
        api_key: &str,
        upload_id: &str,
        chunk_number: u64,
        chunk_hash: &str,
    ) -> Result<UploadMultipartChunkResponse, ApiError> {
        Self::validate_upload_id(upload_id)?;

        // Validate chunk_number is non-zero (S3 multipart uses 1-indexed part numbers)
        if chunk_number == 0 {
            return Err(ApiError::ApiKeyValidationFailure(
                "Chunk number must be greater than 0 (S3 multipart uses 1-indexed part numbers)"
                    .into(),
            ));
        }

        // Validate chunk_hash format - should be non-empty, reasonable length, and valid hex
        if chunk_hash.is_empty() {
            return Err(ApiError::ApiKeyValidationFailure(
                "Chunk hash cannot be empty".into(),
            ));
        }
        if chunk_hash.len() > 128 {
            return Err(ApiError::ApiKeyValidationFailure(
                "Chunk hash exceeds maximum length of 128 characters".into(),
            ));
        }
        if !chunk_hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(ApiError::ApiKeyValidationFailure(
                "Chunk hash must contain only hexadecimal characters (0-9, a-f, A-F)".into(),
            ));
        }
        // Validate chunk_hash has even length (hex strings must have even length to represent complete bytes)
        if chunk_hash.len() % 2 != 0 {
            return Err(ApiError::ApiKeyValidationFailure(
                "Chunk hash must have even length (each byte requires 2 hex characters)".into(),
            ));
        }

        #[derive(Serialize, Debug)]
        struct UploadMultipartChunkRequest<'a> {
            upload_id: &'a str,
            chunk_number: u64,
            chunk_hash: &'a str,
        }

        let response = self
            .client
            .post(format!(
                "{}/tracker/upload/game_control/multipart/chunk",
                API_BASE_URL.as_str()
            ))
            .header("Content-Type", "application/json")
            .header("X-API-Key", api_key)
            .json(&UploadMultipartChunkRequest {
                upload_id,
                chunk_number,
                chunk_hash,
            })
            .send()
            .await?;

        let response =
            check_for_response_success(response, "Upload multipart chunk request failed")
                .await?
                .json::<UploadMultipartChunkResponse>()
                .await?;

        // Validate chunk_number matches what was requested to prevent data integrity issues
        if response.chunk_number != chunk_number {
            return Err(ApiError::ServerInvalidation(format!(
                "Server returned wrong chunk_number: expected {}, got {}",
                chunk_number, response.chunk_number
            )));
        }

        // Validate expires_at to prevent integer overflow when cast to i64 in downstream code
        if response.expires_at > i64::MAX as u64 {
            return Err(ApiError::ServerInvalidation(format!(
                "Server returned expires_at too large: {} (max: {})",
                response.expires_at,
                i64::MAX as u64
            )));
        }

        Ok(response)
    }

    pub async fn complete_multipart_upload(
        &self,
        api_key: &str,
        upload_id: &str,
        chunk_etags: &[CompleteMultipartUploadChunk],
    ) -> Result<CompleteMultipartUploadResponse, ApiError> {
        Self::validate_upload_id(upload_id)?;

        // Validate chunk_etags is not empty to prevent meaningless API calls
        if chunk_etags.is_empty() {
            return Err(ApiError::ApiKeyValidationFailure(
                "Chunk ETags list cannot be empty".into(),
            ));
        }

        #[derive(Serialize, Debug)]
        struct CompleteMultipartUploadRequest<'a> {
            upload_id: &'a str,
            chunk_etags: &'a [CompleteMultipartUploadChunk],
        }

        let response = self
            .client
            .post(format!(
                "{}/tracker/upload/game_control/multipart/complete",
                API_BASE_URL.as_str()
            ))
            .header("Content-Type", "application/json")
            .header("X-API-Key", api_key)
            .json(&CompleteMultipartUploadRequest {
                upload_id,
                chunk_etags,
            })
            .send()
            .await?;

        let response = check_for_response_success(response, "Complete upload request failed")
            .await?
            .json::<CompleteMultipartUploadResponse>()
            .await?;

        if !response.success {
            return Err(ApiError::ApiFailure {
                context: "Complete upload request failed".into(),
                error: response.message,
                status: None,
            });
        }

        Ok(response)
    }

    pub async fn abort_multipart_upload(
        &self,
        api_key: &str,
        upload_id: &str,
    ) -> Result<AbortMultipartUploadResponse, ApiError> {
        Self::validate_upload_id(upload_id)?;
        let encoded_upload_id = utf8_percent_encode(upload_id, NON_ALPHANUMERIC).to_string();
        let response = self
            .client
            .delete(format!(
                "{}/tracker/upload/game_control/multipart/abort/{encoded_upload_id}",
                API_BASE_URL.as_str()
            ))
            .header("X-API-Key", api_key)
            .send()
            .await?;

        let response = check_for_response_success(response, "Abort upload request failed")
            .await?
            .json::<AbortMultipartUploadResponse>()
            .await?;

        if !response.success {
            return Err(ApiError::ApiFailure {
                context: "Abort upload request failed".into(),
                error: response.message,
                status: None,
            });
        }

        Ok(response)
    }
}
