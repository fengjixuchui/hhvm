(*
 * Copyright (c) 2015, Facebook, Inc.
 * All rights reserved.
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the "hack" directory of this source tree.
 *
 *)

(** The file extensions we are interested, each in the form ".ext" *)
val extensions : string list

val is_dot_file : string -> bool

val is_hack : string -> bool

val has_ancestor : string -> string -> bool

(** Filter the relative path. This will reject files not in [extensions] and maybe more. *)
val file_filter : string -> bool

val path_filter : Relative_path.t -> bool
