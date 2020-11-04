<?hh
/**
 * Copyright (c) 2020, Facebook, Inc.
 * All rights reserved.
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the "hack" directory of this source tree.
 *
 */

namespace HH\Capabilities {
  /**
   * The capability for non-determinism
   */
  <<__Sealed()>>
  interface NonDet extends Server {}
}

namespace HH\Contexts {
  type non_det = \HH\Capabilities\NonDet;

  namespace Unsafe {
    type non_det = mixed;
  }
}
