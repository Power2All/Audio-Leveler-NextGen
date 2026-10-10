//! Carry the source file's tags (title, artist, album, track number, cover art, …) over to
//! the output, converted to the output format's native tag type (ID3v2 for MP3/WAV/AAC,
//! Vorbis comments for FLAC/OGG/Opus). Uses lofty, which is pure Rust.

use anyhow::{Context, Result};
use lofty::config::WriteOptions;
use lofty::file::FileType;
use lofty::prelude::*;
use std::path::Path;

/// Copy tags from `src` to the already-encoded `dst`. Does nothing if `src` has no tags.
pub fn copy(src: &Path, dst: &Path, dst_type: FileType) -> Result<()> {
    let tagged = lofty::read_from_path(src).context("reading source tags")?;
    let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) else {
        return Ok(());
    };
    let mut tag = tag.clone();
    // Drop tags that describe the old audio rather than the music: ReplayGain values (the
    // loudness has just changed, so players would turn the new file up or down wrongly) and
    // the encoder that made the source file.
    for key in [
        ItemKey::ReplayGainTrackGain,
        ItemKey::ReplayGainTrackPeak,
        ItemKey::ReplayGainAlbumGain,
        ItemKey::ReplayGainAlbumPeak,
        ItemKey::EncoderSoftware,
        ItemKey::EncoderSettings,
    ] {
        tag.remove_key(&key);
    }
    // Items the destination tag type can't represent are dropped here.
    tag.re_map(dst_type.primary_tag_type());
    if tag.is_empty() {
        return Ok(());
    }
    tag.save_to_path(dst, WriteOptions::default())
        .context("writing tags")?;
    Ok(())
}
