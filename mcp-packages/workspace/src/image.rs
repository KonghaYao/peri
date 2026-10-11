use std::io::Read;

use base64::Engine as _;
use rmcp::model::{CustomRequest, CustomResult};
use serde::{Deserialize, Serialize};

pub const READ_IMAGE_METHOD: &str = "image/read";
pub const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadImageRequest {
    pub path: String,
    pub max_size: usize,
}

#[derive(Serialize, Deserialize)]
pub struct ImageData {
    pub data: String,
    pub media_type: String,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImageReadError {
    NotFound,
    NotFile,
    CannotRead,
    TooLarge,
    NotImage,
}

pub type ReadImageResponse = Result<ImageData, ImageReadError>;

pub(crate) async fn handle_read_image(
    cwd: &str,
    request: CustomRequest,
) -> Result<CustomResult, rmcp::ErrorData> {
    let params: ReadImageRequest = serde_json::from_value(request.params.ok_or_else(|| {
        rmcp::ErrorData::invalid_params("image/read requires path and max_size", None)
    })?)
    .map_err(|_| rmcp::ErrorData::invalid_params("invalid image/read parameters", None))?;
    let cwd = cwd.to_owned();
    let result = tokio::task::spawn_blocking(move || load_image(&cwd, params))
        .await
        .map_err(|_| rmcp::ErrorData::internal_error("image read failed", None))?;
    let value = serde_json::to_value(result)
        .map_err(|_| rmcp::ErrorData::internal_error("image response encoding failed", None))?;
    Ok(CustomResult::new(value))
}

fn load_image(cwd: &str, request: ReadImageRequest) -> ReadImageResponse {
    let expanded = if request.path == "~" || request.path.starts_with("~/") {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .ok_or(ImageReadError::CannotRead)?;
        std::path::PathBuf::from(home).join(request.path.strip_prefix("~/").unwrap_or(""))
    } else {
        std::path::PathBuf::from(&request.path)
    };
    let path = crate::filesystem::resolve_path(cwd, &expanded.to_string_lossy());
    let limit = request.max_size.min(MAX_IMAGE_BYTES);
    let metadata = std::fs::metadata(&path).map_err(read_error)?;
    if !metadata.is_file() {
        return Err(ImageReadError::NotFile);
    }
    if metadata.len() > limit as u64 {
        return Err(ImageReadError::TooLarge);
    }
    let file = std::fs::File::open(path).map_err(read_error)?;
    if !file.metadata().map_err(read_error)?.is_file() {
        return Err(ImageReadError::NotFile);
    }
    let mut data = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut data)
        .map_err(read_error)?;
    if data.len() > limit {
        return Err(ImageReadError::TooLarge);
    }
    let media_type = match image::guess_format(&data).map_err(|_| ImageReadError::NotImage)? {
        image::ImageFormat::Png => "image/png",
        image::ImageFormat::Jpeg => "image/jpeg",
        image::ImageFormat::Gif => "image/gif",
        image::ImageFormat::WebP => "image/webp",
        _ => return Err(ImageReadError::NotImage),
    };
    Ok(ImageData {
        data: base64::engine::general_purpose::STANDARD.encode(data),
        media_type: media_type.to_owned(),
    })
}

fn read_error(error: std::io::Error) -> ImageReadError {
    if error.kind() == std::io::ErrorKind::NotFound {
        ImageReadError::NotFound
    } else {
        ImageReadError::CannotRead
    }
}

#[cfg(test)]
#[path = "image_test.rs"]
mod tests;
