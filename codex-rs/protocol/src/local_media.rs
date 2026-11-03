use std::io;
use std::io::Read;
use std::path::Path;

use codex_utils_image::MAX_PROMPT_IMAGE_INPUT_BYTES;
use codex_utils_image::PromptImageMode;
use codex_utils_image::load_for_prompt_bytes;

use crate::models::ImageDetail;
use crate::models::ImageReference;
use crate::user_input::UserInput;

/// Snapshots local image input into portable, validated data URLs.
pub fn snapshot_local_user_input(input: &mut UserInput) -> io::Result<()> {
    match input {
        UserInput::LocalImage { path, detail } => {
            let image_detail = *detail;
            let mode = match image_detail {
                Some(ImageDetail::Original) => PromptImageMode::Original,
                Some(ImageDetail::Auto | ImageDetail::Low | ImageDetail::High) | None => {
                    PromptImageMode::ResizeToFit
                }
            };
            let file_bytes = read_bounded_local_media(path, MAX_PROMPT_IMAGE_INPUT_BYTES, "image")?;
            let image = load_for_prompt_bytes(path, file_bytes, mode).map_err(io::Error::other)?;
            *input = UserInput::Image {
                image: ImageReference::Inline {
                    image_url: image.into_data_url(),
                },
                detail: image_detail,
            };
        }
        UserInput::Text { .. }
        | UserInput::Image { .. }
        | UserInput::Skill { .. }
        | UserInput::Mention { .. } => {}
    }
    Ok(())
}

fn read_bounded_local_media(path: &Path, max_bytes: usize, kind: &str) -> io::Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() > max_bytes as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{kind} input exceeds {max_bytes} bytes"),
        ));
    }

    let mut bytes = Vec::new();
    file.take(max_bytes as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{kind} input exceeds {max_bytes} bytes"),
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
#[path = "local_media_tests.rs"]
mod tests;
