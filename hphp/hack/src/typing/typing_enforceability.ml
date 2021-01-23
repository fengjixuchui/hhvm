(*
 * Copyright (c) 2017, Facebook, Inc.
 * All rights reserved.
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the "hack" directory of this source tree.
 *
 *)

open Hh_prelude
open Typing_defs
open Typing_env_types
module Cls = Decl_provider.Class
module MakeType = Typing_make_type
module Env = Typing_env

let wrap_like ty =
  let r = Typing_reason.Renforceable (get_pos ty) in
  MakeType.like r ty

let is_enforceable (env : env) (ty : decl_ty) =
  let rec is_enforceable env visited ty =
    match get_node ty with
    | Tthis -> false
    | Tapply ((_, name), _)
      when Env.is_enum env name
           && not (TypecheckerOptions.enable_sound_dynamic env.genv.tcopt) ->
      false
    | Tapply ((_, name), tyl) ->
      (* Cyclic type definition error will be produced elsewhere *)
      SSet.mem name visited
      ||
      begin
        match Env.get_class_or_typedef env name with
        | Some
            (Env.TypedefResult
              { td_vis = Aast.Transparent; td_tparams; td_type; _ }) ->
          (* So that the check does not collide with reified generics *)
          let env = Env.add_generic_parameters env td_tparams in
          is_enforceable env (SSet.add name visited) td_type
        | Some (Env.ClassResult tc) ->
          begin
            match tyl with
            | [] -> true
            | targs ->
              List.Or_unequal_lengths.(
                begin
                  match
                    List.fold2
                      ~init:true
                      targs
                      (Cls.tparams tc)
                      ~f:(fun acc targ tparam ->
                        match get_node targ with
                        | Tdynamic
                        (* We accept the inner type being dynamic regardless of reification *)
                        | Tlike _ ->
                          acc
                        | _ ->
                          (match tparam.tp_reified with
                          | Aast.Erased -> false
                          | Aast.SoftReified -> false
                          | Aast.Reified ->
                            is_enforceable env visited targ && acc))
                  with
                  | Ok new_acc -> new_acc
                  | Unequal_lengths -> true
                end)
          end
        | _ -> false
      end
    | Tgeneric _ ->
      (* Previously we allowed dynamic ~> T when T is an __Enforceable generic,
       * that is, when it's valid on the RHS of an `is` or `as` expression.
       * However, `is` / `as` checks have different behavior than runtime checks
       * for `tuple`s and `shapes`s; `is` / `as` will shallow-ly check declared
       * fields but typehint enforcement only checks that we have the right
       * array type (`varray` for `tuple`, `darray` for `shape`). This means
       * it's unsound to allow this coercion.
       *
       * Additionally, higher kinded generics (i.e., with type arguments) cannot
       * be enforced at the moment; they are disallowed to have upper bounds.
       *)
      false
    | Taccess _ -> false
    | Tlike _ -> false
    | Tprim prim ->
      begin
        match prim with
        | Aast.Tvoid
        | Aast.Tnoreturn ->
          false
        | _ -> true
      end
    | Tany _ -> true
    | Terr -> true
    | Tnonnull -> true
    | Tdynamic -> true
    | Tfun _ -> false
    | Ttuple _ -> false
    | Tunion [] -> true
    | Tunion _ -> false
    | Tintersection _ -> false
    | Tshape _ -> false
    | Tmixed -> true
    | Tvar _ -> false
    | Tdarray _ -> false
    | Tvarray _ -> false
    (* With no parameters, we enforce varray_or_darray just like array *)
    | Tvarray_or_darray (_, ty) -> is_any ty
    | Toption ty -> is_enforceable env visited ty
    (* TODO(T36532263) make sure that this is what we want *)
  in
  is_enforceable env SSet.empty ty

let make_locl_like_type env ty =
  if env.Typing_env_types.pessimize then
    let dyn = MakeType.dynamic (Reason.Renforceable (get_pos ty)) in
    Typing_union.union env dyn ty
  else
    (env, ty)

let is_enforced env ~explicitly_untrusted ty =
  let enforceable = is_enforceable env ty in
  let is_hhi =
    get_pos ty
    |> Pos.filename
    |> Relative_path.prefix
    |> Relative_path.(equal_prefix Hhi)
  in
  enforceable && (not is_hhi) && not explicitly_untrusted

let pessimize_type env { et_type; et_enforced } =
  let et_type =
    if et_enforced || not env.pessimize then
      et_type
    else
      match get_node et_type with
      | Tprim (Aast.Tvoid | Aast.Tnoreturn) -> et_type
      | _ -> wrap_like et_type
  in
  { et_type; et_enforced }

let compute_enforced_ty env ?(explicitly_untrusted = false) (ty : decl_ty) =
  let et_enforced = is_enforced env ~explicitly_untrusted ty in
  { et_type = ty; et_enforced }

let compute_enforced_and_pessimize_ty
    env ?(explicitly_untrusted = false) (ty : decl_ty) =
  let ety = compute_enforced_ty env ~explicitly_untrusted ty in
  pessimize_type env ety

let handle_awaitable_return env ft_fun_kind (ft_ret : decl_possibly_enforced_ty)
    =
  let { et_type = return_type; _ } = ft_ret in
  match (ft_fun_kind, get_node return_type) with
  | (Ast_defs.FAsync, Tapply ((_, name), [inner_ty]))
    when String.equal name Naming_special_names.Classes.cAwaitable ->
    let { et_enforced; _ } = compute_enforced_ty env inner_ty in
    pessimize_type env { et_type = return_type; et_enforced }
  | _ -> compute_enforced_and_pessimize_ty env return_type

let compute_enforced_and_pessimize_fun_type env (ft : decl_fun_type) =
  let { ft_params; ft_ret; _ } = ft in
  let ft_fun_kind = get_ft_fun_kind ft in
  let ft_ret = handle_awaitable_return env ft_fun_kind ft_ret in
  let ft_params =
    List.map
      ~f:(fun fp ->
        let { fp_type = { et_type; _ }; _ } = fp in
        let f =
          if equal_param_mode (get_fp_mode fp) FPinout then
            compute_enforced_and_pessimize_ty
          else
            compute_enforced_ty
        in
        let fp_type = f env et_type in
        { fp with fp_type })
      ft_params
  in
  { ft with ft_params; ft_ret }
