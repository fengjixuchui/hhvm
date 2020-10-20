// Copyright (c) 2019, Facebook, Inc.
// All rights reserved.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the "hack" directory of this source tree.

use bumpalo::Bump;

pub trait HasArena<'a> {
    fn get_arena(&self) -> &'a Bump;
}
