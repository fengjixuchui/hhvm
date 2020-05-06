// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the "hack" directory of this source tree.

pub use oxidized_by_ref::typing_reason::{ArgPosition, Reason as PReason_, Reason_ as Reason};

pub type PReason<'a> = &'a PReason_<'a>;
