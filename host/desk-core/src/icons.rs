//! The key icons built into the panel: Lucide shapes as 4-bit coverage,
//! tinted on the panel with the key's text colour (`tools/icons/gen.py`).
//!
//! deskd validates `icon` names against `NAMES` and the panel draws from the
//! same table, so a name deskd accepts is one the panel has.

mod names;

pub use names::{BITS, NAMES, SIZE};

const BYTES: usize = SIZE as usize * SIZE as usize * BITS as usize / 8;
static DATA: &[u8] = include_bytes!("icons/icons.bin");

/// An icon's bitmap: `SIZE` rows of `SIZE` pixels, two per byte, high
/// nibble first.
pub fn get(name: &str) -> Option<&'static [u8]> {
    let i = NAMES.binary_search(&name).ok()?;
    DATA.get(i * BYTES..(i + 1) * BYTES)
}

/// Coverage of pixel (x, y), 0 (none) to 15 (full).
pub fn alpha(bitmap: &[u8], x: u16, y: u16) -> u8 {
    let i = y as usize * SIZE as usize + x as usize;
    let byte = bitmap[i / 2];
    if i.is_multiple_of(2) { byte >> 4 } else { byte & 0x0F }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_matches_the_bitmaps() {
        assert_eq!(DATA.len(), NAMES.len() * BYTES);
        assert!(NAMES.windows(2).all(|w| w[0] < w[1]), "sorted and unique, for binary search");
    }

    #[test]
    fn every_icon_has_a_shape() {
        for name in NAMES {
            let icon = get(name).unwrap();
            let inked = (0..SIZE).flat_map(|y| (0..SIZE).map(move |x| (x, y))).filter(|&(x, y)| alpha(icon, x, y) > 0);
            assert!(inked.count() > 20, "{name}");
        }
        assert!(get("no-such-icon").is_none());
    }
}
