<?hh
/**
 * Copyright (c) 2020, Facebook, Inc.
 * All rights reserved.
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the "hack" directory of this source tree.
 *
 */

/**
 * To concisely model multiple capabilities, we use intersection types
 * (denoted by &). This is an exact approximation, unlike introducing
 * a new subtype via interface hierarchies and using multiple inheritance.
 * (In case of the latter, introducing all parent capabilities separately
 * would not be equal to introducing them via the subtype capability.)
 */
<<file:__EnableUnstableFeatures('union_intersection_type_hints')>>

/**
* This namespace provides a mapping between user-facing contexts
* and a set of capabilities that determine the operations allowed (semantics).
* Contexts have snake-case name and appear in user code (syntax).
* Capabilities are modeled as a series of sealed interfaces:
* - extends clause that lists all other capabilities that are subsumed,
*   i.e., automatically present with the subtyped capability
*/
namespace HH\Contexts {

  /**
   * The default, normally unannotated context. This is currently hardcoded in
   * Typing_make_type.default_capability for performance reasons. The alias is
   * still present so that it may be directly used as [defaults]
   */
  type defaults = nothing; // an infinite set of all capabilities
  // TODO(coeffects) after implementing lower bounds on const ctx/type, do:
  /* = (
    \HH\Capabilities\WriteProperty &
    \HH\Capabilities\AccessStaticVariable &
    \HH\Capabilities\IO
  ); */

  // TODO(cipp): deal with not giving it WriteProperty (or some other mechanism of turning on IFC)
  type cipp_global = (
    \HH\Capabilities\AccessStaticVariable &
    \HH\Capabilities\IO &
    \HH\Capabilities\WriteProperty
  );
  // type cipp = (\HH\Capabilities\Cipp & cipp_global);
  type cipp = (
    \HH\Capabilities\Cipp &
    \HH\Capabilities\AccessStaticVariable &
    \HH\Capabilities\IO &
    \HH\Capabilities\WriteProperty
  );
  // type cipp_of<T> = (\HH\Capabilities\Cipp_of<T> & cipp);
  type cipp_of<T> = (
    \HH\Capabilities\Cipp_of<T> &
    \HH\Capabilities\AccessStaticVariable &
    \HH\Capabilities\IO &
    \HH\Capabilities\WriteProperty
  );

  type write_props = \HH\Capabilities\WriteProperty;

  type rx = (\HH\Capabilities\Rx & \HH\Capabilities\WriteProperty);
  // type rx_shallow = (\HH\Capabilities\RxShallow & rx);
  type rx_shallow = (\HH\Capabilities\RxShallow & \HH\Capabilities\WriteProperty);
  // type rx_local = (\HH\Capabilities\RxLocal & rx_shallow);
  type rx_local = (\HH\Capabilities\RxLocal & \HH\Capabilities\WriteProperty);
}
