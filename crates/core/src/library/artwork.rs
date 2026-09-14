//! Embedded cover art, pulled from file tags via `lofty`.
//!
//! The desktop Playout screen shows the art next to the now-playing
//! label. The Icecast/Shoutcast source protocol carries text-only
//! in-band metadata (`StreamTitle`), so there is deliberately no
//! encoder sink here — "where supported" (v4 P3.7) is the desktop UI
//! (and any future remote UI reading [`crate::audio::Engine`]), not
//! the stream. Nothing is ever written back to files.

use std::path::Path;

/// Cap for one embedded picture: station libraries sometimes carry
/// multi-megabyte scans, and the art rides in memory beside the deck.
pub const MAX_ARTWORK_BYTES: usize = 512 * 1024;

/// One embedded picture, ready for an image-capable consumer.
#[derive(Debug, Clone)]
pub struct Artwork {
    /// MIME type as stored (`image/jpeg`, …).
    pub mime: String,
    /// Raw encoded bytes (JPEG/PNG/…, decoded by the consumer).
    pub data: Vec<u8>,
}

/// First usable picture across all tags: a front cover when tagged,
/// else whatever picture comes first. Skips empty blobs and anything
/// past [`MAX_ARTWORK_BYTES`]. Pure over in-memory tags, so tests pin
/// the selection without touching disk.
pub(crate) fn pick_artwork(tags: &[lofty::tag::Tag]) -> Option<Artwork> {
    let mut fallback: Option<&lofty::picture::Picture> = None;
    for tag in tags {
        for pic in tag.pictures() {
            if pic.data().is_empty() || pic.data().len() > MAX_ARTWORK_BYTES {
                continue;
            }
            if pic.pic_type() == lofty::picture::PictureType::CoverFront {
                return Some(artwork_of(pic));
            }
            if fallback.is_none() {
                fallback = Some(pic);
            }
        }
    }
    fallback.map(artwork_of)
}

fn artwork_of(pic: &lofty::picture::Picture) -> Artwork {
    Artwork {
        mime: pic
            .mime_type()
            .map(|m| m.as_str().to_string())
            .unwrap_or_else(|| "image/jpeg".to_string()),
        data: pic.data().to_vec(),
    }
}

/// Cover art embedded in `path`, if any. Reads tags only (no decode);
/// safe on the background loader thread. `None` covers missing files,
/// untagged audio, and oversized pictures alike — the caller shows no
/// art, it never errors.
pub fn artwork_for(path: &Path) -> Option<Artwork> {
    use lofty::file::TaggedFileExt;
    let tagged = lofty::read_from_path(path).ok()?;
    pick_artwork(tagged.tags())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lofty::picture::{MimeType, Picture, PictureType};
    use lofty::tag::{Tag, TagType};

    fn picture(kind: PictureType, mime: MimeType, data: &[u8]) -> Picture {
        Picture::new_unchecked(kind, Some(mime), None, data.to_vec())
    }

    fn tagged(pics: Vec<Picture>) -> Vec<Tag> {
        let mut tag = Tag::new(TagType::Id3v2);
        for p in pics {
            tag.push_picture(p);
        }
        vec![tag]
    }

    #[test]
    fn pick_prefers_front_cover_over_first() {
        let pics = vec![
            picture(PictureType::Artist, MimeType::Png, b"artist"),
            picture(PictureType::CoverFront, MimeType::Jpeg, b"cover"),
        ];
        let art = pick_artwork(&tagged(pics)).expect("cover wins");
        assert_eq!(art.mime, "image/jpeg");
        assert_eq!(art.data, b"cover");
    }

    #[test]
    fn pick_falls_back_to_first_usable() {
        let pics = vec![picture(PictureType::CoverBack, MimeType::Png, b"back")];
        let art = pick_artwork(&tagged(pics)).expect("fallback");
        assert_eq!(art.mime, "image/png");
    }

    #[test]
    fn pick_skips_empty_and_oversized() {
        let big = vec![0xFF; MAX_ARTWORK_BYTES + 1];
        let pics = vec![
            picture(PictureType::CoverFront, MimeType::Jpeg, &[]),
            picture(PictureType::CoverFront, MimeType::Jpeg, &big),
            picture(PictureType::CoverFront, MimeType::Jpeg, b"ok"),
        ];
        // Empty + oversized skipped inside the same tag.
        let art = pick_artwork(&tagged(pics)).expect("third picture");
        assert_eq!(art.data, b"ok");
        assert!(pick_artwork(&tagged(vec![])).is_none());
        assert!(pick_artwork(&[]).is_none());
    }

    /// Real file roundtrip: silent MP3 carrier, ID3v2 APIC written by
    /// lofty itself, read back through [`artwork_for`].
    #[test]
    fn artwork_for_reads_tagged_mp3() {
        use lofty::tag::TagExt;
        let dir = std::env::temp_dir().join(format!("crabboss-art-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("song.mp3");
        std::fs::write(&path, silent_mp3()).unwrap();
        let mut tag = Tag::new(TagType::Id3v2);
        tag.push_picture(picture(
            PictureType::CoverFront,
            MimeType::Jpeg,
            b"fake-jpeg",
        ));
        tag.save_to_path(&path, lofty::config::WriteOptions::default())
            .expect("tag write works");
        let art = artwork_for(&path).expect("art found");
        assert_eq!(art.mime, "image/jpeg");
        assert_eq!(art.data, b"fake-jpeg");
        // Same carrier without a tag: no art, no error.
        let bare = dir.join("bare.mp3");
        std::fs::write(&bare, silent_mp3()).unwrap();
        assert!(artwork_for(&bare).is_none());
        // Missing file: no art, no error.
        assert!(artwork_for(&dir.join("gone.mp3")).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// ~0.2 s of LAME silence: enough MPEG frames for lofty to probe.
    /// Buffer dance copied from `Mp3Encoder::encode`.
    fn silent_mp3() -> Vec<u8> {
        use mp3lame_encoder::{Builder, InterleavedPcm};
        let mut builder = Builder::new().expect("lame");
        builder.set_num_channels(2).unwrap();
        builder.set_sample_rate(44100).unwrap();
        builder
            .set_brate(mp3lame_encoder::Bitrate::Kbps128)
            .unwrap();
        let mut enc = builder.build().unwrap();
        let mut frame: Vec<u8> = Vec::with_capacity(16_384);
        let scratch = vec![0i16; 2 * 1152];
        let mut out = Vec::new();
        for _ in 0..8 {
            frame.clear();
            let n = enc
                .encode(InterleavedPcm(&scratch), frame.spare_capacity_mut())
                .unwrap();
            unsafe {
                frame.set_len(frame.len().wrapping_add(n));
            }
            out.extend_from_slice(&frame);
        }
        frame.clear();
        let n = enc
            .flush::<mp3lame_encoder::FlushNoGap>(frame.spare_capacity_mut())
            .unwrap();
        unsafe {
            frame.set_len(frame.len().wrapping_add(n));
        }
        out.extend_from_slice(&frame);
        out
    }
}
