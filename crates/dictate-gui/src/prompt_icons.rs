//! Prompt artwork, import drafts, and immutable application-owned icon resources.
//!
//! Imports stay in memory until the outer settings save. `PreparedIcons` removes only
//! files it created if that save fails; a successful save commits those resources.
//! Existing valid files are never replaced or deleted by this module.

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use dictate_core::rewrite::PromptIcon;

mod builtin;
use builtin::pixels as builtin_pixels;
use sha2::{Digest, Sha256};

const MAX_IMPORT_BYTES: u64 = 4 * 1024 * 1024;
const MAX_IMAGE_SIDE: u32 = 4096;
const MAX_DECODE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ICON_SIDE: u32 = 256;
const MAX_CACHE_ENTRIES: usize = 256;

#[derive(Clone)]
pub struct IconDraft {
    root: PathBuf,
    pending: HashMap<String, Arc<Vec<u8>>>,
}

impl IconDraft {
    pub fn new(config_path: &Path) -> Self {
        Self {
            root: config_root(config_path),
            pending: HashMap::new(),
        }
    }

    /// Validate completely before changing the draft. An invalid new image cannot
    /// replace the previous preview or damage an already saved icon.
    pub fn stage(&mut self, source: &Path) -> Result<PromptIcon, String> {
        let extension = image_extension(source)?;
        let bytes = read_bounded(source)?;
        let _ = decode(&bytes, extension, 64)?;
        let digest = Sha256::digest(&bytes);
        let reference = format!("icons/{digest:x}.{extension}");
        self.pending.insert(reference.clone(), Arc::new(bytes));
        Ok(PromptIcon::Custom(reference))
    }

    pub fn absorb(&mut self, other: Self) {
        debug_assert_eq!(self.root, other.root);
        self.pending.extend(other.pending);
    }

    /// Call immediately before saving the final configuration; retain the returned
    /// guard until the asynchronous save completes, then call `commit` on success.
    pub fn prepare<'a>(
        &self,
        icons: impl IntoIterator<Item = &'a PromptIcon>,
    ) -> Result<PreparedIcons, String> {
        let referenced: HashSet<&str> = icons
            .into_iter()
            .filter_map(|icon| match icon {
                PromptIcon::Custom(reference) => Some(reference.as_str()),
                _ => None,
            })
            .collect();
        let mut prepared = PreparedIcons {
            created: Vec::new(),
        };
        for (reference, bytes) in &self.pending {
            if !referenced.contains(reference.as_str()) {
                continue;
            }
            let path = managed_path(&self.root, reference)?;
            let directory = path.parent().ok_or("Invalid icon directory")?;
            fs::create_dir_all(directory).map_err(|error| error.to_string())?;
            reject_resource_symlinks(&self.root, &path)?;
            // Never overwrite an existing immutable resource, even if it is damaged.
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    prepared.created.push(path);
                    file.write_all(bytes).map_err(|error| error.to_string())?;
                    file.sync_all().map_err(|error| error.to_string())?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let existing = read_bounded(&path)?;
                    if existing.as_slice() != bytes.as_slice() {
                        return Err("An icon resource with this name has different contents".into());
                    }
                }
                Err(error) => return Err(error.to_string()),
            }
        }
        Ok(prepared)
    }
}

pub struct PreparedIcons {
    created: Vec<PathBuf>,
}

impl PreparedIcons {
    pub fn commit(mut self) {
        self.created.clear();
    }
}

impl Drop for PreparedIcons {
    fn drop(&mut self) {
        for path in &self.created {
            let _ = fs::remove_file(path);
        }
    }
}

fn config_root(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

fn image_extension(path: &Path) -> Result<&'static str, String> {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => Ok("png"),
        "jpg" | "jpeg" => Ok("jpg"),
        "svg" => Ok("svg"),
        _ => Err("Choose a PNG, JPG, JPEG, or SVG image".into()),
    }
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, String> {
    let file = fs::File::open(path).map_err(|error| error.to_string())?;
    if file.metadata().map_err(|error| error.to_string())?.len() > MAX_IMPORT_BYTES {
        return Err("Icon files must be no larger than 4 MiB".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_IMPORT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_IMPORT_BYTES {
        return Err("Icon image is empty or larger than 4 MiB".into());
    }
    Ok(bytes)
}

fn managed_path(root: &Path, reference: &str) -> Result<PathBuf, String> {
    // Check both Windows and Unix separators on every host, including Linux tests.
    let name = reference
        .strip_prefix("icons/")
        .ok_or("Icon reference must be inside the managed icons directory")?;
    if name.is_empty()
        || !name.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '.' || character == '-'
        })
        || name.starts_with('.')
        || name.contains("..")
    {
        return Err("Invalid icon resource reference".into());
    }
    let path = root.join("icons").join(name);
    image_extension(&path)?;
    Ok(path)
}

fn reject_resource_symlinks(root: &Path, path: &Path) -> Result<(), String> {
    for candidate in [root.join("icons"), path.to_path_buf()] {
        match fs::symlink_metadata(candidate) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err("Managed icon resources must not be symbolic links".into());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}

/// A square transparent canvas, premultiplied BGRA for Direct2D and AlphaBlend.
/// The artwork is scaled proportionally and centered inside the canvas.
pub struct IconPixels {
    pub size: u32,
    pub bgra: Vec<u8>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    icon: PromptIcon,
    size: u32,
    staged: bool,
}

pub struct IconCache {
    root: PathBuf,
    pixels: HashMap<CacheKey, Arc<IconPixels>>,
}

impl IconCache {
    pub fn new(config_path: &Path) -> Self {
        Self {
            root: config_root(config_path),
            pixels: HashMap::new(),
        }
    }

    pub fn pixels(
        &mut self,
        icon: &PromptIcon,
        size: u32,
        _dark: bool,
        draft: Option<&IconDraft>,
    ) -> Arc<IconPixels> {
        let size = size.clamp(1, MAX_ICON_SIDE);
        let key = CacheKey {
            icon: icon.clone(),
            size,
            staged: is_staged(icon, draft),
        };
        if let Some(pixels) = self.pixels.get(&key) {
            return Arc::clone(pixels);
        }
        let pixels = match icon {
            PromptIcon::Custom(reference) => self
                .custom_pixels(reference, size, draft)
                .unwrap_or_else(|_| builtin_pixels(None, size)),
            PromptIcon::Builtin(builtin) => builtin_pixels(Some(*builtin), size),
            PromptIcon::Default => builtin_pixels(None, size),
        };
        let pixels = Arc::new(pixels);
        if self.pixels.len() >= MAX_CACHE_ENTRIES {
            self.pixels.clear();
        }
        self.pixels.insert(key, Arc::clone(&pixels));
        pixels
    }

    fn custom_pixels(
        &self,
        reference: &str,
        size: u32,
        draft: Option<&IconDraft>,
    ) -> Result<IconPixels, String> {
        let path = managed_path(&self.root, reference)?;
        let extension = image_extension(&path)?;
        if let Some(bytes) = draft.and_then(|draft| draft.pending.get(reference)) {
            return decode(bytes, extension, size);
        }
        reject_resource_symlinks(&self.root, &path)?;
        decode(&read_bounded(&path)?, extension, size)
    }
}

fn is_staged(icon: &PromptIcon, draft: Option<&IconDraft>) -> bool {
    matches!(icon, PromptIcon::Custom(reference) if draft.is_some_and(|draft| draft.pending.contains_key(reference)))
}

fn decode(bytes: &[u8], extension: &str, size: u32) -> Result<IconPixels, String> {
    if extension == "svg" {
        return decode_svg(bytes, size);
    }
    let format = match extension {
        "png" => image::ImageFormat::Png,
        "jpg" => image::ImageFormat::Jpeg,
        _ => return Err("Unsupported icon format".into()),
    };
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_SIDE);
    limits.max_image_height = Some(MAX_IMAGE_SIDE);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);
    let mut image = reader
        .decode()
        .map_err(|error| error.to_string())?
        .into_rgba8();
    // Scale premultiplied channels so transparent edge colors cannot bleed into
    // the visible artwork or create dark fringes on the acrylic backdrop.
    for pixel in image.pixels_mut() {
        let alpha = u16::from(pixel[3]);
        for channel in &mut pixel.0[..3] {
            *channel = ((u16::from(*channel) * alpha + 127) / 255) as u8;
        }
    }
    let resized = image::DynamicImage::ImageRgba8(image).resize(
        size,
        size,
        image::imageops::FilterType::Lanczos3,
    );
    let rgba = resized.to_rgba8();
    let mut bgra = vec![0; (size * size * 4) as usize];
    let offset_x = (size - rgba.width()) / 2;
    let offset_y = (size - rgba.height()) / 2;
    for (x, y, pixel) in rgba.enumerate_pixels() {
        let destination = (((y + offset_y) * size + x + offset_x) * 4) as usize;
        for (index, channel) in [pixel[2], pixel[1], pixel[0]].into_iter().enumerate() {
            bgra[destination + index] = channel.min(pixel[3]);
        }
        bgra[destination + 3] = pixel[3];
    }
    Ok(IconPixels { size, bgra })
}

fn decode_svg(bytes: &[u8], size: u32) -> Result<IconPixels, String> {
    // Never use usvg's default string resolver: it can open arbitrary local files.
    // Embedded PNG/JPEG images pass the same format and allocation checks before
    // resvg receives them. Nested SVG data URLs and every external href are denied.
    let mut options = resvg::usvg::Options {
        image_href_resolver: resvg::usvg::ImageHrefResolver {
            resolve_string: Box::new(|_, _| None),
            resolve_data: Box::new(|mime, bytes, _| {
                if bytes.len() as u64 > MAX_IMPORT_BYTES {
                    return None;
                }
                let (extension, image) = match mime {
                    "image/png" => ("png", resvg::usvg::ImageKind::PNG(Arc::clone(&bytes))),
                    "image/jpeg" | "image/jpg" => {
                        ("jpg", resvg::usvg::ImageKind::JPEG(Arc::clone(&bytes)))
                    }
                    _ => return None,
                };
                decode(&bytes, extension, 1).ok().map(|_| image)
            }),
        },
        ..Default::default()
    };
    // Loading system fonts is expensive; vector-only glyphs never need it. The
    // immutable database is shared across custom text icons and DPI changes.
    if bytes.windows(5).any(|window| window == b"<text") {
        static FONT_DATABASE: OnceLock<Arc<resvg::usvg::fontdb::Database>> = OnceLock::new();
        options.fontdb = Arc::clone(FONT_DATABASE.get_or_init(|| {
            let mut database = resvg::usvg::fontdb::Database::new();
            database.load_system_fonts();
            Arc::new(database)
        }));
    }
    let tree = resvg::usvg::Tree::from_data(bytes, &options)
        .map_err(|error| format!("Invalid SVG icon: {error}"))?;
    if tree.root().children().is_empty() {
        return Err("SVG icon does not contain supported vector artwork".into());
    }
    let dimensions = tree.size();
    if dimensions.width() > MAX_IMAGE_SIDE as f32 || dimensions.height() > MAX_IMAGE_SIDE as f32 {
        return Err("SVG dimensions must not exceed 4096 pixels".into());
    }
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size).ok_or("Invalid icon size")?;
    let scale = (size as f32 / dimensions.width()).min(size as f32 / dimensions.height());
    let transform = resvg::tiny_skia::Transform::from_row(
        scale,
        0.0,
        0.0,
        scale,
        (size as f32 - dimensions.width() * scale) / 2.0,
        (size as f32 - dimensions.height() * scale) / 2.0,
    );
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    let mut bgra = pixmap.take();
    for pixel in bgra.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    Ok(IconPixels { size, bgra })
}

#[cfg(windows)]
mod windows_render;
#[cfg(windows)]
pub use windows_render::{IconRenderer, draw_gdi};

#[cfg(test)]
mod tests;
