use dictate_core::rewrite::BuiltinPromptIcon;

use super::*;

const SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="16"><rect width="32" height="16" fill="#e02040"/></svg>"##;

fn source(directory: &Path, name: &str) -> PathBuf {
    let path = directory.join(name);
    fs::write(&path, SVG).unwrap();
    path
}

fn custom_reference(icon: &PromptIcon) -> &str {
    match icon {
        PromptIcon::Custom(reference) => reference,
        _ => panic!("expected custom icon"),
    }
}

#[test]
fn imports_remain_in_memory_until_outer_save_and_rollback_on_failure() {
    let directory = tempfile::tempdir().unwrap();
    let mut draft = IconDraft::new(&directory.path().join("config.json"));
    let icon = draft
        .stage(&source(directory.path(), "source.svg"))
        .unwrap();
    let path = directory.path().join(custom_reference(&icon));
    assert!(!path.exists());
    {
        let _prepared = draft.prepare([&icon]).unwrap();
        assert!(path.exists());
        // Simulate main settings save failure or cancellation.
    }
    assert!(!path.exists());
    draft.prepare([&icon]).unwrap().commit();
    assert_eq!(fs::read(&path).unwrap(), SVG);
}

#[test]
fn nested_editor_cancel_discards_imports_and_save_transfers_them() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config.json");
    let mut main = IconDraft::new(&config);
    let mut editor = main.clone();
    let icon = editor
        .stage(&source(directory.path(), "source.svg"))
        .unwrap();
    drop(editor);
    assert!(main.pending.is_empty());
    assert!(!directory.path().join("icons").exists());
    let mut editor = main.clone();
    editor.stage(&directory.path().join("source.svg")).unwrap();
    main.absorb(editor);
    main.prepare([&icon]).unwrap().commit();
    assert!(directory.path().join(custom_reference(&icon)).exists());
}

#[test]
fn unreferenced_imports_are_not_written_and_existing_resources_survive_rollback() {
    let directory = tempfile::tempdir().unwrap();
    let mut draft = IconDraft::new(&directory.path().join("config.json"));
    let icon = draft
        .stage(&source(directory.path(), "source.svg"))
        .unwrap();
    draft.prepare([]).unwrap().commit();
    assert!(!directory.path().join("icons").exists());
    draft.prepare([&icon]).unwrap().commit();
    drop(draft.prepare([&icon]).unwrap());
    assert!(directory.path().join(custom_reference(&icon)).exists());
}

#[test]
fn damaged_existing_resource_is_never_overwritten() {
    let directory = tempfile::tempdir().unwrap();
    let mut draft = IconDraft::new(&directory.path().join("config.json"));
    let icon = draft
        .stage(&source(directory.path(), "source.svg"))
        .unwrap();
    fs::create_dir(directory.path().join("icons")).unwrap();
    let path = directory.path().join(custom_reference(&icon));
    fs::write(&path, b"previous bytes").unwrap();
    assert!(draft.prepare([&icon]).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"previous bytes");
}

#[test]
fn import_rejects_bad_data_wrong_extension_and_oversized_images() {
    let directory = tempfile::tempdir().unwrap();
    let mut draft = IconDraft::new(&directory.path().join("config.json"));
    assert!(draft.stage(&source(directory.path(), "wrong.png")).is_err());
    assert!(draft.stage(&source(directory.path(), "wrong.txt")).is_err());
    let large = directory.path().join("large.svg");
    fs::write(&large, vec![0; MAX_IMPORT_BYTES as usize + 1]).unwrap();
    assert!(draft.stage(&large).is_err());
    assert!(
        decode_svg(
            br#"<svg width="999999" height="2"><rect width="1" height="1"/></svg>"#,
            32
        )
        .is_err()
    );
    assert!(draft.pending.is_empty());
}

#[test]
fn relative_resources_reject_traversal_and_windows_paths_on_all_hosts() {
    for reference in [
        "../test.png",
        "icons/../test.png",
        "icons/test/other.png",
        "icons/..\\test.png",
        "C:\\test.png",
        "icons/C:test.png",
        "icons/.secret.png",
        "/icons/test.png",
    ] {
        assert!(
            managed_path(Path::new("."), reference).is_err(),
            "{reference}"
        );
    }
    assert!(managed_path(Path::new("."), "icons/abc123.png").is_ok());
}

#[cfg(unix)]
#[test]
fn symlink_resources_are_not_loaded_or_written() {
    let directory = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), directory.path().join("icons")).unwrap();
    let mut draft = IconDraft::new(&directory.path().join("config.json"));
    let icon = draft
        .stage(&source(directory.path(), "source.svg"))
        .unwrap();
    assert!(draft.prepare([&icon]).is_err());
    assert!(fs::read_dir(outside.path()).unwrap().next().is_none());
}

#[test]
fn custom_images_keep_aspect_color_alpha_and_cache_without_source_file() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config.json");
    let mut draft = IconDraft::new(&config);
    let source = source(directory.path(), "source.svg");
    let icon = draft.stage(&source).unwrap();
    fs::remove_file(source).unwrap();
    let mut cache = IconCache::new(&config);
    let pixels = cache.pixels(&icon, 32, false, Some(&draft));
    assert!(pixels.bgra[..32 * 4].iter().all(|value| *value == 0));
    let center = (16 * 32 + 16) * 4;
    assert_eq!(&pixels.bgra[center..center + 4], &[0x40, 0x20, 0xe0, 0xff]);
    let dark = cache.pixels(&icon, 32, true, Some(&draft));
    assert!(Arc::ptr_eq(&pixels, &dark));
    draft.prepare([&icon]).unwrap().commit();
    let persisted = cache.pixels(&icon, 32, false, None);
    fs::remove_file(directory.path().join(custom_reference(&icon))).unwrap();
    assert!(Arc::ptr_eq(
        &pixels,
        &cache.pixels(&icon, 32, false, Some(&draft))
    ));
    assert!(Arc::ptr_eq(
        &persisted,
        &cache.pixels(&icon, 32, false, None)
    ));
}

#[test]
fn png_and_jpeg_imports_decode_with_centered_alpha_and_original_colors() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config.json");
    let mut draft = IconDraft::new(&config);
    let mut cache = IconCache::new(&config);
    for (format, name) in [
        (image::ImageFormat::Png, "test.PNG"),
        (image::ImageFormat::Jpeg, "test.JPEG"),
    ] {
        let source = directory.path().join(name);
        let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            20,
            10,
            image::Rgba([240, 80, 20, 128]),
        ));
        let image = if format == image::ImageFormat::Jpeg {
            image::DynamicImage::ImageRgb8(image.to_rgb8())
        } else {
            image
        };
        image.save_with_format(&source, format).unwrap();
        let icon = draft.stage(&source).unwrap();
        let pixels = cache.pixels(&icon, 40, false, Some(&draft));
        assert!(pixels.bgra[..40 * 4].iter().all(|value| *value == 0));
        let center = (20 * 40 + 20) * 4;
        let center = &pixels.bgra[center..center + 4];
        if format == image::ImageFormat::Png {
            assert_eq!(center, &[10, 40, 120, 128]);
        } else {
            assert_eq!(center[3], 255);
            assert!(center[2] > center[1] && center[1] > center[0]);
        }
    }
}

#[test]
fn importing_a_missing_resource_replaces_cached_fallback_in_the_draft() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config.json");
    let mut draft = IconDraft::new(&config);
    let mut cache = IconCache::new(&config);
    let reference = format!("icons/{:x}.svg", Sha256::digest(SVG));
    let missing = PromptIcon::Custom(reference);
    let fallback = cache.pixels(&missing, 32, false, None);
    let icon = draft
        .stage(&source(directory.path(), "source.svg"))
        .unwrap();
    let imported = cache.pixels(&icon, 32, false, Some(&draft));
    assert_ne!(fallback.bgra, imported.bgra);
}

#[test]
fn svg_embedded_png_is_validated_and_rendered_without_external_access() {
    use base64::Engine;
    let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        2,
        2,
        image::Rgba([255, 0, 0, 255]),
    ));
    let mut bytes = Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    let data = base64::engine::general_purpose::STANDARD.encode(bytes.into_inner());
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32"><image href="data:image/png;base64,{data}" width="32" height="32"/></svg>"#
    );
    let pixels = decode_svg(svg.as_bytes(), 32).unwrap();
    assert_eq!(
        &pixels.bgra[(16 * 32 + 16) * 4..(16 * 32 + 16) * 4 + 4],
        &[0, 0, 255, 255]
    );
}

#[test]
fn missing_corrupt_and_invalid_resources_fall_back_without_affecting_other_icons() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config.json");
    fs::create_dir(directory.path().join("icons")).unwrap();
    fs::write(directory.path().join("icons/bad.png"), b"bad").unwrap();
    let mut cache = IconCache::new(&config);
    let default = cache.pixels(&PromptIcon::Default, 30, false, None);
    for reference in ["icons/missing.png", "icons/bad.png", "../escape.svg"] {
        let pixels = cache.pixels(&PromptIcon::Custom(reference.into()), 30, false, None);
        assert_eq!(default.bgra, pixels.bgra);
    }
    let valid = cache.pixels(
        &PromptIcon::Builtin(BuiltinPromptIcon::Messages),
        30,
        false,
        None,
    );
    assert_ne!(default.bgra, valid.bgra);
}

#[test]
fn svg_external_images_are_not_loaded() {
    let directory = tempfile::tempdir().unwrap();
    let path = source(directory.path(), "external.svg");
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32"><image href="{}" width="32" height="32"/></svg>"#,
        path.display()
    );
    assert!(decode_svg(svg.as_bytes(), 32).is_err());
}

#[test]
fn builtin_vectors_render_at_each_monitor_scale() {
    let mut cache = IconCache::new(Path::new("config.json"));
    let icons = std::iter::once(PromptIcon::Default)
        .chain(BuiltinPromptIcon::ALL.into_iter().map(PromptIcon::Builtin));
    for icon in icons {
        for size in [22, 28, 33, 44, 66] {
            let pixels = cache.pixels(&icon, size, false, None);
            let dark = cache.pixels(&icon, size, true, None);
            assert!(
                Arc::ptr_eq(&pixels, &dark),
                "{icon:?}: theme must reuse artwork"
            );
            assert_eq!(pixels.bgra.len(), (size * size * 4) as usize);
            let samples = pixels.bgra.as_chunks::<4>().0;
            assert!(
                samples
                    .iter()
                    .all(|pixel| pixel[..3].iter().all(|channel| *channel <= pixel[3]))
            );
            assert!(samples.iter().any(|pixel| pixel[3] > 0 && pixel[3] < 255));
            // The supplied palette survives in both themes, including at 100% DPI.
            // Test premultiplied values without requiring opaque pixels in thin paths.
            for rgb in [[0x64u16, 0xb5, 0xf6], [0x1e, 0x88, 0xe5]] {
                assert!(
                    samples.iter().any(|pixel| {
                        pixel[3] >= 64
                            && [rgb[2], rgb[1], rgb[0]].iter().zip(&pixel[..3]).all(
                                |(channel, actual)| {
                                    let expected = (channel * u16::from(pixel[3]) + 127) / 255;
                                    expected.abs_diff(u16::from(*actual)) <= 2
                                },
                            )
                    }),
                    "{icon:?} at {size}px lost palette color {rgb:?}"
                );
            }
            let mut left = size;
            let mut top = size;
            let mut right = 0;
            let mut bottom = 0;
            for (index, pixel) in samples.iter().enumerate() {
                let x = index as u32 % size;
                let y = index as u32 / size;
                if x == 0 || y == 0 || x + 1 == size || y + 1 == size {
                    assert!(pixel[3] <= 8, "{icon:?} at {size}px clips an outer edge");
                }
                if pixel[3] > 8 {
                    left = left.min(x);
                    top = top.min(y);
                    right = right.max(x);
                    bottom = bottom.max(y);
                }
            }
            assert!(left <= right && top <= bottom, "{icon:?} is empty");
            assert!(
                (right - left + 1).max(bottom - top + 1) as f32 >= size as f32 * 0.85,
                "{icon:?} at {size}px retains excess source whitespace"
            );
            assert!(
                (left + right).abs_diff(size - 1) <= 2,
                "{icon:?} is off-center horizontally"
            );
            assert!(
                (top + bottom).abs_diff(size - 1) <= 2,
                "{icon:?} is off-center vertically"
            );
        }
    }
    let unique: HashSet<_> = BuiltinPromptIcon::ALL
        .into_iter()
        .map(|icon| {
            cache
                .pixels(&PromptIcon::Builtin(icon), 44, false, None)
                .bgra
                .clone()
        })
        .collect();
    assert_eq!(unique.len(), BuiltinPromptIcon::ALL.len());
    assert_eq!(
        cache.pixels(&PromptIcon::Default, 44, false, None).bgra,
        cache
            .pixels(
                &PromptIcon::Builtin(BuiltinPromptIcon::Chat),
                44,
                false,
                None
            )
            .bgra,
    );
}
