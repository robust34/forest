// Copyright 2019-2023 ChainSafe Systems
// SPDX-License-Identifier: Apache-2.0, MIT

use crate::utils::cid::CidVariant;
use cid::Cid;
use serde::{Deserialize, Serialize};

/// A wrapper around `Box<[CidVariant]>` with a [`Cid`]-friendly API:
/// - Uses [`CidVariant`] over [`Cid`] to save memory on common CIDs - see docs for that type for more
/// - Uses `Box<[...]>`, over `Vec<...>` avoiding vector overallocation
///
/// There will be MANY small collections of [`FrozenCids`] over the codebase, so these space savings matter
#[cfg_attr(test, derive(derive_quickcheck_arbitrary::Arbitrary))]
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct FrozenCids(
    #[cfg_attr(test, arbitrary(gen(
        |g| Vec::arbitrary(g).into_boxed_slice()
    )))]
    Box<[CidVariant]>,
);

pub struct Iter<'a> {
    cids: std::slice::Iter<'a, CidVariant>,
}

impl<'a> IntoIterator for &'a FrozenCids {
    type Item = Cid;
    type IntoIter = Iter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        Iter {
            cids: self.0.iter(),
        }
    }
}

impl<'a> Iterator for Iter<'a> {
    type Item = Cid;
    fn next(&mut self) -> Option<Self::Item> {
        self.cids.next().map(Cid::from)
    }
}

impl FromIterator<Cid> for FrozenCids {
    fn from_iter<T: IntoIterator<Item = Cid>>(iter: T) -> Self {
        FrozenCids::from(iter.into_iter().collect::<Vec<_>>())
    }
}

impl From<Vec<Cid>> for FrozenCids {
    fn from(cids: Vec<Cid>) -> Self {
        let mut small_cids = Vec::with_capacity(cids.len());
        for cid in cids {
            match cid.into() {
                CidVariant::V1DagCborBlake2b(bytes) => {
                    small_cids.push(CidVariant::V1DagCborBlake2b(bytes))
                }
                _ => small_cids.push(CidVariant::Generic(Box::new(cid))),
            }
        }
        FrozenCids(small_cids.into_boxed_slice())
    }
}

impl From<FrozenCids> for Vec<Cid> {
    fn from(frozen_cids: FrozenCids) -> Self {
        Vec::<Cid>::from(&frozen_cids)
    }
}

impl From<&FrozenCids> for Vec<Cid> {
    fn from(frozen_cids: &FrozenCids) -> Self {
        let mut cids = Vec::with_capacity(frozen_cids.0.len());
        for cid in frozen_cids.into_iter() {
            match cid.into() {
                CidVariant::V1DagCborBlake2b(bytes) => {
                    cids.push(Cid::from(CidVariant::V1DagCborBlake2b(bytes)))
                }
                _ => cids.push(cid),
            }
        }
        cids
    }
}

impl FrozenCids {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn contains(&self, cid: Cid) -> bool {
        let cid = CidVariant::from(cid);
        self.0.contains(&cid)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use quickcheck_macros::quickcheck;

    #[quickcheck]
    fn cidvec_to_vec_of_cids_to_cidvec(cidvec: FrozenCids) {
        assert_eq!(cidvec, FrozenCids::from(Vec::<Cid>::from(cidvec.clone())));
    }

    #[quickcheck]
    fn serialize_vec_of_cids_deserialize_cidvec(vec_of_cids: Vec<Cid>) {
        let serialized = serde_json::to_string(&vec_of_cids).unwrap();
        let parsed: FrozenCids = serde_json::from_str(&serialized).unwrap();
        assert_eq!(vec_of_cids, Vec::<Cid>::from(parsed));
    }

    #[quickcheck]
    fn serialize_cidvec_deserialize_vec_of_cids(cidvec: FrozenCids) {
        let serialized = serde_json::to_string(&cidvec).unwrap();
        let parsed: Vec<Cid> = serde_json::from_str(&serialized).unwrap();
        assert_eq!(Vec::<Cid>::from(cidvec), parsed);
    }
}
