use crate::{CodebaseName, RunId};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use jiff::Timestamp;
use reqwest::{Client, StatusCode};
use rusty_s3::{Bucket, Credentials, S3Action, UrlStyle};
use std::io::{self, Cursor, Read};
use std::path::Path;
use std::time::Duration;

use crate::logs::{Error, LogFileManager};

// How long a signed request URL stays valid.
const SIGNATURE_LIFETIME: Duration = Duration::from_secs(300);

#[derive(Clone, Copy)]
enum Method {
    Head,
    Get,
    Put,
    Delete,
}

pub struct S3LogFileManager {
    bucket: Bucket,
    credentials: Option<Credentials>,
    client: Client,
}

impl S3LogFileManager {
    /// Signs requests with AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY when both are set.
    pub fn new(endpoint_url: &str, bucket_name: Option<&str>) -> Result<Self, Error> {
        let region = std::env::var("AWS_REGION")
            .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
            .unwrap_or_else(|_| "us-east-1".to_string());
        Self::with_credentials(endpoint_url, bucket_name, &region, Credentials::from_env())
    }

    pub fn with_credentials(
        endpoint_url: &str,
        bucket_name: Option<&str>,
        region: &str,
        credentials: Option<Credentials>,
    ) -> Result<Self, Error> {
        let bucket_name = bucket_name.unwrap_or("debian-janitor");
        let mut endpoint = url::Url::parse(endpoint_url)
            .map_err(|e| Error::Other(format!("Invalid S3 URL: {}", e)))?;
        // Without the trailing slash, joining the bucket name drops the last path segment.
        if !endpoint.path().ends_with('/') {
            let path = format!("{}/", endpoint.path());
            endpoint.set_path(&path);
        }
        let bucket = Bucket::new(
            endpoint,
            UrlStyle::Path,
            bucket_name.to_string(),
            region.to_string(),
        )
        .map_err(|e| Error::Other(e.to_string()))?;

        let client = Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .map_err(|e| Error::Other(e.to_string()))?;

        Ok(Self {
            bucket,
            credentials,
            client,
        })
    }

    fn get_key(
        &self,
        codebase: &CodebaseName,
        run_id: &RunId,
        name: &str,
    ) -> Result<String, Error> {
        // URL parsing resolves a .. segment, which would leave logs/.
        if name
            .split('/')
            .any(|segment| segment.is_empty() || segment == "..")
        {
            return Err(Error::NotFound);
        }
        Ok(format!("logs/{}/{}/{}.gz", codebase, run_id, name))
    }

    fn object_url(&self, method: Method, key: &str, time: &Timestamp) -> url::Url {
        let credentials = self.credentials.as_ref();
        match method {
            Method::Head => self
                .bucket
                .head_object(credentials, key)
                .sign_with_time(SIGNATURE_LIFETIME, time),
            Method::Get => self
                .bucket
                .get_object(credentials, key)
                .sign_with_time(SIGNATURE_LIFETIME, time),
            Method::Put => {
                let mut action = self.bucket.put_object(credentials, key);
                action.headers_mut().insert("x-amz-acl", "public-read");
                action.sign_with_time(SIGNATURE_LIFETIME, time)
            }
            Method::Delete => self
                .bucket
                .delete_object(credentials, key)
                .sign_with_time(SIGNATURE_LIFETIME, time),
        }
    }

    fn get_url(
        &self,
        method: Method,
        codebase: &CodebaseName,
        run_id: &RunId,
        name: &str,
    ) -> Result<url::Url, Error> {
        let key = self.get_key(codebase, run_id, name)?;
        Ok(self.object_url(method, &key, &Timestamp::now()))
    }
}

#[async_trait]
impl LogFileManager for S3LogFileManager {
    async fn has_log(
        &self,
        codebase: &CodebaseName,
        run_id: &RunId,
        name: &str,
    ) -> Result<bool, Error> {
        let url = match self.get_url(Method::Head, codebase, run_id, name) {
            Ok(url) => url,
            Err(_) => return Ok(false),
        };

        match self.client.head(url).send().await {
            Ok(resp) => match resp.status() {
                StatusCode::OK => Ok(true),
                StatusCode::NOT_FOUND => Ok(false),
                StatusCode::FORBIDDEN => Ok(false),
                status => Err(Error::Other(format!(
                    "Unexpected response code: {}",
                    status
                ))),
            },
            Err(_e) => Err(Error::ServiceUnavailable),
        }
    }

    async fn get_log(
        &self,
        codebase: &CodebaseName,
        run_id: &RunId,
        name: &str,
    ) -> Result<Box<dyn Read + Send + Sync>, Error> {
        let url = self.get_url(Method::Get, codebase, run_id, name)?;

        let resp = self
            .client
            .get(url)
            .timeout(Duration::from_secs(300))
            .send()
            .await
            .map_err(|_| Error::ServiceUnavailable)?;

        match resp.status() {
            StatusCode::OK => {
                let bytes = resp.bytes().await.map_err(|_| Error::ServiceUnavailable)?;
                let cursor = Cursor::new(bytes.to_vec());
                let decoder = GzDecoder::new(cursor);
                Ok(Box::new(decoder))
            }
            StatusCode::NOT_FOUND => Err(Error::NotFound),
            StatusCode::FORBIDDEN => Err(Error::PermissionDenied),
            status => Err(Error::Other(format!(
                "Unexpected response code: {}",
                status
            ))),
        }
    }

    async fn import_log(
        &self,
        codebase: &CodebaseName,
        run_id: &RunId,
        orig_path: &str,
        _mtime: Option<DateTime<Utc>>,
        basename: Option<&str>,
    ) -> Result<(), Error> {
        let data = tokio::fs::read(orig_path).await?;

        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        io::copy(&mut Cursor::new(&data), &mut encoder)?;
        let compressed_data = encoder.finish()?;

        let basename = basename.unwrap_or_else(|| {
            Path::new(orig_path)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
        });

        let url = self.get_url(Method::Put, codebase, run_id, basename)?;

        let resp = self
            .client
            .put(url)
            .header("x-amz-acl", "public-read")
            .body(compressed_data)
            .send()
            .await
            .map_err(|_| Error::ServiceUnavailable)?;

        match resp.status() {
            StatusCode::OK | StatusCode::CREATED => Ok(()),
            StatusCode::FORBIDDEN => Err(Error::PermissionDenied),
            StatusCode::SERVICE_UNAVAILABLE => Err(Error::ServiceUnavailable),
            status => Err(Error::Other(format!(
                "Upload failed with status: {}",
                status
            ))),
        }
    }

    async fn delete_log(
        &self,
        codebase: &CodebaseName,
        run_id: &RunId,
        name: &str,
    ) -> Result<(), Error> {
        let url = self.get_url(Method::Delete, codebase, run_id, name)?;

        let resp = self
            .client
            .delete(url)
            .send()
            .await
            .map_err(|_| Error::ServiceUnavailable)?;

        match resp.status() {
            StatusCode::OK | StatusCode::NO_CONTENT => Ok(()),
            StatusCode::NOT_FOUND => Err(Error::NotFound),
            StatusCode::FORBIDDEN => Err(Error::PermissionDenied),
            status => Err(Error::Other(format!(
                "Delete failed with status: {}",
                status
            ))),
        }
    }

    async fn iter_logs(&self) -> Box<dyn Iterator<Item = (String, String, Vec<String>)>> {
        // S3 listing is complex and would require XML parsing
        // For now, return empty iterator - this can be implemented later
        Box::new(std::iter::empty())
    }

    async fn get_ctime(
        &self,
        codebase: &CodebaseName,
        run_id: &RunId,
        name: &str,
    ) -> Result<DateTime<Utc>, Error> {
        let url = self.get_url(Method::Head, codebase, run_id, name)?;

        // Use HEAD request to get object metadata including last-modified
        let response = self
            .client
            .head(url)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|_| Error::ServiceUnavailable)?;

        if !response.status().is_success() {
            match response.status().as_u16() {
                404 => return Err(Error::NotFound),
                _ => return Err(Error::ServiceUnavailable),
            }
        }

        // Try to get last-modified header as creation time approximation
        if let Some(last_modified) = response.headers().get("last-modified") {
            if let Ok(date_str) = last_modified.to_str() {
                // Parse RFC 2822 format (HTTP date format)
                if let Ok(datetime) = DateTime::parse_from_rfc2822(date_str) {
                    return Ok(datetime.with_timezone(&Utc));
                }
            }
        }

        // If no reliable creation time is available, return an error
        Err(Error::Other(
            "Creation time not available for S3 object".to_string(),
        ))
    }

    async fn health_check(&self) -> Result<(), Error> {
        // Try to access the bucket root
        let resp = self
            .client
            .head(
                self.bucket
                    .head_bucket(self.credentials.as_ref())
                    .sign(SIGNATURE_LIFETIME),
            )
            .timeout(Duration::from_secs(10))
            .send()
            .await;

        match resp {
            Ok(r) if r.status().is_success() => Ok(()),
            Ok(r) if r.status() == StatusCode::FORBIDDEN => Err(Error::PermissionDenied),
            _ => Err(Error::ServiceUnavailable),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids() -> (CodebaseName, RunId) {
        (
            CodebaseName::new("cb").unwrap(),
            RunId::new("run1").unwrap(),
        )
    }

    #[test]
    fn unsigned_without_credentials() {
        let manager = S3LogFileManager::with_credentials(
            "http://s3.example:3900/prefix",
            None,
            "garage",
            None,
        )
        .unwrap();
        let (codebase, run_id) = ids();
        assert_eq!(
            manager
                .get_url(Method::Get, &codebase, &run_id, "build.log")
                .unwrap()
                .as_str(),
            "http://s3.example:3900/prefix/debian-janitor/logs/cb/run1/build.log.gz"
        );
    }

    #[test]
    fn signed_with_credentials() {
        let manager = S3LogFileManager::with_credentials(
            "http://s3.example:3900",
            Some("logs-bucket"),
            "garage",
            Some(Credentials::new("GKkey", "secret")),
        )
        .unwrap();
        let (codebase, run_id) = ids();
        let key = manager.get_key(&codebase, &run_id, "build.log").unwrap();
        let time = Timestamp::from_second(1_800_000_000).unwrap();
        assert_eq!(
            manager.object_url(Method::Put, &key, &time).as_str(),
            "http://s3.example:3900/logs-bucket/logs/cb/run1/build.log.gz?X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=GKkey%2F20270115%2Fgarage%2Fs3%2Faws4_request&X-Amz-Date=20270115T080000Z&X-Amz-Expires=300&X-Amz-SignedHeaders=host%3Bx-amz-acl&X-Amz-Signature=120e741ca7f5efe0a31e9171e3139caeb7f339895483f108c66bb88381e0ac4e"
        );
    }

    #[test]
    fn dot_dot_and_empty_segments_rejected() {
        let manager =
            S3LogFileManager::with_credentials("http://s3.example:3900", None, "garage", None)
                .unwrap();
        let (codebase, run_id) = ids();
        for name in ["../../../other-bucket/secret", "a//build.log", ""] {
            for method in [Method::Get, Method::Put] {
                assert_eq!(
                    manager
                        .get_url(method, &codebase, &run_id, name)
                        .map(|url| url.to_string())
                        .map_err(|e| e.to_string()),
                    Err("Not found".to_string())
                );
            }
        }
    }
}
