//! KOReader-style partial MD5, identical to Readest `utils/md5.ts::partialMD5`.
//! Samples 1 KiB at offset 0 and at `1024 << (2*i)` for i in 0..=10.

use md5::{Digest, Md5};
use std::io::{Read, Seek, SeekFrom};

const SAMPLE: usize = 1024;

pub fn partial_md5<R: Read + Seek>(r: &mut R) -> std::io::Result<String> {
    let size = r.seek(SeekFrom::End(0))?;
    let mut hasher = Md5::new();
    let mut offsets: Vec<u64> = vec![0];
    offsets.extend((0..=10).map(|i| 1024u64 << (2 * i)));
    let mut buf = vec![0u8; SAMPLE];
    for start in offsets {
        if start >= size {
            break;
        }
        let end = (start + SAMPLE as u64).min(size);
        let len = (end - start) as usize;
        r.seek(SeekFrom::Start(start))?;
        r.read_exact(&mut buf[..len])?;
        hasher.update(&buf[..len]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn empty_file_is_md5_of_nothing() {
        let h = partial_md5(&mut Cursor::new(Vec::<u8>::new())).unwrap();
        assert_eq!(h, "d41d8cd98f00b204e9800998ecf8427e");
    }

    #[test]
    fn small_file_hashes_whole_content() {
        let h = partial_md5(&mut Cursor::new(b"hello".to_vec())).unwrap();
        assert_eq!(h, "5d41402abc4b2a76b9719d911017c592");
    }
}
