// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the "hack" directory of this source tree.
//
// @generated SignedSource<<e18a57e5e575c413af85ef3c0d957da5>>
//
// To regenerate this file, run:
//   hphp/hack/src/oxidized_regen.sh

use arena_trait::TrivialDrop;
use no_pos_hash::NoPosHash;
use ocamlrep_derive::FromOcamlRepIn;
use ocamlrep_derive::ToOcamlRep;
use serde::Serialize;

#[allow(unused_imports)]
use crate::*;

#[derive(
    Clone,
    Debug,
    Eq,
    FromOcamlRepIn,
    Hash,
    NoPosHash,
    Ord,
    PartialEq,
    PartialOrd,
    Serialize,
    ToOcamlRep
)]
pub struct DeclParserOptions<'a> {
    pub array_unification: bool,
    pub auto_namespace_map: &'a [(&'a str, &'a str)],
    pub disable_xhp_element_mangling: bool,
    pub interpret_soft_types_as_like_types: bool,
}
impl<'a> TrivialDrop for DeclParserOptions<'a> {}
