(*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the "hack" directory of this source tree.
 *
 *)

val apply_rules :
  ?ignore_type_structure:bool ->
  Typing_env_types.env ->
  Typing_defs.locl_ty ->
  (Typing_env_types.env ->
  Typing_defs.locl_ty ->
  Typing_env_types.env * Typing_defs.locl_ty) ->
  Typing_env_types.env * Typing_defs.locl_ty
