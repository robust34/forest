// Copyright 2019-2023 ChainSafe Systems
// SPDX-License-Identifier: Apache-2.0, MIT
use positioned_io::ReadAt;
use std::io::Result;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
#[cfg_attr(test, derive(derive_quickcheck_arbitrary::Arbitrary))]
pub struct IndexHeader {
    // Version number
    pub magic_number: u64,
    // Worst-case distance between an entry and its bucket.
    pub longest_distance: u64,
    // Number of hash collisions. Reserved for future use.
    pub collisions: u64,
    // Number of buckets. Note that the index includes padding after the last
    // bucket.
    pub buckets: u64,
}

impl IndexHeader {
    pub const SIZE: usize = 32;
    // 0xdeadbeef + 0 used a different hash algorithm
    pub const MAGIC_NUMBER: u64 = 0xdeadbeef + 1;

    pub fn read(reader: impl ReadAt, offset: u64) -> Result<IndexHeader> {
        let mut buffer = [0; Self::SIZE];
        reader.read_exact_at(offset, &mut buffer)?;
        Ok(IndexHeader::from_le_bytes(buffer))
    }

    pub fn to_le_bytes(self) -> [u8; IndexHeader::SIZE] {
        let mut bytes = [0; IndexHeader::SIZE];
        bytes[0..8].copy_from_slice(&self.magic_number.to_le_bytes());
        bytes[8..16].copy_from_slice(&self.longest_distance.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.collisions.to_le_bytes());
        bytes[24..32].copy_from_slice(&self.buckets.to_le_bytes());
        bytes
    }

    pub fn from_le_bytes(bytes: [u8; IndexHeader::SIZE]) -> Self {
        IndexHeader {
            magic_number: u64::from_le_bytes(bytes[0..8].try_into().expect("infallible")),
            longest_distance: u64::from_le_bytes(bytes[8..16].try_into().expect("infallible")),
            collisions: u64::from_le_bytes(bytes[16..24].try_into().expect("infallible")),
            buckets: u64::from_le_bytes(bytes[24..32].try_into().expect("infallible")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quickcheck_macros::quickcheck;

    #[quickcheck]
    fn index_header_roundtrip(header: IndexHeader) {
        assert_eq!(header, IndexHeader::from_le_bytes(header.to_le_bytes()))
    }
}
