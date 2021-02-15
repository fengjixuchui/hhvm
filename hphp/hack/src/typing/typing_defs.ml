(*
 * Copyright (c) 2015, Facebook, Inc.
 * All rights reserved.
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the "hack" directory of this source tree.
 *
 *)

open Hh_prelude
open Typing_defs_flags
include Typing_defs_core

type const_decl = {
  cd_pos: Pos.t;
  cd_type: decl_ty;
}
[@@deriving show]

type class_elt = {
  ce_visibility: ce_visibility;
  ce_type: decl_ty Lazy.t;
  ce_origin: string;  (** identifies the class from which this elt originates *)
  ce_deprecated: string option;
  ce_pos: Pos.t Lazy.t;  (** pos of the type of the elt *)
  ce_flags: int;
}
[@@deriving show]

type fun_elt = {
  fe_deprecated: string option;
  fe_type: decl_ty;
  fe_pos: Pos.t;
  fe_php_std_lib: bool;
}
[@@deriving show]

type class_const = {
  cc_synthesized: bool;
  cc_abstract: bool;
  cc_pos: Pos.t;
  cc_type: decl_ty;
  cc_origin: string;
      (** identifies the class from which this const originates *)
}
[@@deriving show]

type record_field_req =
  | ValueRequired
  | HasDefaultValue
[@@deriving show]

type record_def_type = {
  rdt_name: Nast.sid;
  rdt_extends: Nast.sid option;
  rdt_fields: (Nast.sid * record_field_req) list;
  rdt_abstract: bool;
  rdt_pos: Pos.t;
}
[@@deriving show]

(** The position is that of the hint in the `use` / `implements` AST node
 * that causes a class to have this requirement applied to it. E.g.
 *
 * ```
 * class Foo {}
 *
 * interface Bar {
 *   require extends Foo; <- position of the decl_phase ty
 * }
 *
 * class Baz extends Foo implements Bar { <- position of the `implements`
 * }
 * ```
 *)
type requirement = Pos.t * decl_ty

and class_type = {
  tc_need_init: bool;
  tc_members_fully_known: bool;
      (** Whether the typechecker knows of all (non-interface) ancestors
       * and thus knows all accessible members of this class
       * This is not the case if one ancestor at least could not be found. *)
  tc_abstract: bool;
  tc_final: bool;
  tc_const: bool;
  tc_deferred_init_members: SSet.t;
      (** When a class is abstract (or in a trait) the initialization of
       * a protected member can be delayed *)
  tc_kind: Ast_defs.class_kind;
  tc_is_xhp: bool;
  tc_has_xhp_keyword: bool;
  tc_is_disposable: bool;
  tc_name: string;
  tc_pos: Pos.t;
  tc_tparams: decl_tparam list;
  tc_where_constraints: decl_where_constraint list;
  tc_consts: class_const SMap.t;
  tc_typeconsts: typeconst_type SMap.t;
  tc_props: class_elt SMap.t;
  tc_sprops: class_elt SMap.t;
  tc_methods: class_elt SMap.t;
  tc_smethods: class_elt SMap.t;
  tc_construct: class_elt option * consistent_kind;
      (** the consistent_kind represents final constructor or __ConsistentConstruct *)
  tc_ancestors: decl_ty SMap.t;
      (** This includes all the classes, interfaces and traits this class is
       * using. *)
  tc_implements_dynamic: bool;  (** Whether the class is coercible to dynamic *)
  tc_req_ancestors: requirement list;
  tc_req_ancestors_extends: SSet.t;  (** the extends of req_ancestors *)
  tc_extends: SSet.t;
  tc_enum_type: enum_type option;
  tc_sealed_whitelist: SSet.t option;
  tc_decl_errors: Errors.t option; [@opaque]
}

and typeconst_abstract_kind =
  | TCAbstract of decl_ty option
  | TCPartiallyAbstract
  | TCConcrete

and typeconst_type = {
  ttc_abstract: typeconst_abstract_kind;
  ttc_name: Nast.sid;
  ttc_as_constraint: decl_ty option;
  ttc_type: decl_ty option;
  ttc_origin: string;
  ttc_enforceable: Pos.t * bool;
  ttc_reifiable: Pos.t option;
}

and enum_type = {
  te_base: decl_ty;
  te_constraint: decl_ty option;
  te_includes: decl_ty list;
  te_enum_class: bool;
}
[@@deriving show]

type typedef_type = {
  td_pos: Pos.t;
  td_vis: Aast.typedef_visibility;
  td_tparams: decl_tparam list;
  td_constraint: decl_ty option;
  td_type: decl_ty;
}
[@@deriving show]

let is_enum_class = function
  | None -> false
  | Some info -> info.te_enum_class

type phase_ty =
  | DeclTy of decl_ty
  | LoclTy of locl_ty

type deserialization_error =
  | Wrong_phase of string
      (** The type was valid, but some component thereof was a decl_ty when we
          expected a locl_phase ty, or vice versa. *)
  | Not_supported of string
      (** The specific type or some component thereof is not one that we support
          deserializing, usually because not enough information was serialized to be
          able to deserialize it again. *)
  | Deserialization_error of string
      (** The input JSON was invalid for some reason. *)

(** Tracks information about how a type was expanded *)
type expand_env = {
  type_expansions: (bool * Pos.t * string) list;
      (** A list of the type defs and type access we have expanded thus far. Used
       * to prevent entering into a cycle when expanding these types.
       * If the boolean is set, then emit an error because we were checking the
       * definition of a type (by type, or newtype, or a type constant)
       *)
  substs: locl_ty SMap.t;
  this_ty: locl_ty;
      (** The type that is substituted for `this` in signatures. It should be
       * set to an expression dependent type if appropraite
       *)
  quiet: bool;
      (** If set to true, do not report errors, just return Terr or equivalent *)
  on_error: Errors.typing_error_callback;
      (** If what we are localizing or expanding comes from the decl heap for
          example, then some errors must be silenced since they must have already been
          raised when first typechecking whatever we have fetched from the heap.
          Setting {!quiet} to true will silence those errors.
          T54121530 aims at offering a better mechanism. *)
}

let get_var t =
  match get_node t with
  | Tvar v -> Some v
  | _ -> None

let get_class_type t =
  match get_node t with
  | Tclass (id, exact, tyl) -> Some (id, exact, tyl)
  | _ -> None

let get_var_i t =
  match t with
  | LoclType t -> get_var t
  | ConstraintType _ -> None

let is_tyvar t = Option.is_some (get_var t)

let is_var_v t v =
  match get_node t with
  | Tvar v' when Ident.equal v v' -> true
  | _ -> false

let is_generic t =
  match get_node t with
  | Tgeneric _ -> true
  | _ -> false

let is_dynamic t =
  match get_node t with
  | Tdynamic -> true
  | _ -> false

let is_nonnull t =
  match get_node t with
  | Tnonnull -> true
  | _ -> false

let is_fun t =
  match get_node t with
  | Tfun _ -> true
  | _ -> false

let is_any t =
  match get_node t with
  | Tany _ -> true
  | _ -> false

let is_generic_equal_to n t =
  (* TODO(T69551141) handle type arguments *)
  match get_node t with
  | Tgeneric (n', _tyargs) when String.equal n n' -> true
  | _ -> false

let is_prim p t =
  match get_node t with
  | Tprim p' when Aast.equal_tprim p p' -> true
  | _ -> false

let is_union t =
  match get_node t with
  | Tunion _ -> true
  | _ -> false

let is_constraint_type_union t =
  match deref_constraint_type t with
  | (_, TCunion _) -> true
  | _ -> false

let is_has_member t =
  match deref_constraint_type t with
  | (_, Thas_member _) -> true
  | _ -> false

let show_phase_ty _ = "<phase_ty>"

let pp_phase_ty _ _ = Printf.printf "%s\n" "<phase_ty>"

let is_locl_type = function
  | LoclType _ -> true
  | _ -> false

let has_expanded { type_expansions; _ } x =
  List.find_map type_expansions (function
      | (report, _, x') when String.equal x x' -> Some report
      | _ -> None)

let reason = function
  | LoclType t -> get_reason t
  | ConstraintType t -> fst (deref_constraint_type t)

let is_constraint_type = function
  | ConstraintType _ -> true
  | LoclType _ -> false

let is_union_or_inter_type (ty : locl_ty) =
  (* do not expand type here! *)
  match get_node ty with
  | Toption _
  | Tunion _
  | Tintersection _ ->
    true
  | Terr
  | Tnonnull
  | Tdynamic
  | Tobject
  | Tany _
  | Tprim _
  | Tfun _
  | Ttuple _
  | Tshape _
  | Tvar _
  | Tnewtype _
  | Tdependent _
  | Tgeneric _
  | Tclass _
  | Tvarray _
  | Tdarray _
  | Tunapplied_alias _
  | Tvarray_or_darray _
  | Tvec_or_dict _
  | Taccess _ ->
    false

module InternalType = struct
  let get_var t =
    match t with
    | LoclType t -> get_var t
    | ConstraintType _ -> None

  let is_var_v t ~v =
    match t with
    | LoclType t -> is_var_v t v
    | ConstraintType _ -> false

  let is_not_var_v t ~v = not @@ is_var_v t ~v
end

(* The identifier for this *)
let this = Local_id.make_scoped "$this"

(* This should be the ONLY way that Tany is constructed anywhere in the
 * codebase. *)
let make_tany () = Tany TanySentinel.value

let arity_min ft : int =
  List.count ~f:(fun fp -> not (get_fp_has_default fp)) ft.ft_params

let get_param_mode callconv =
  match callconv with
  | Some Ast_defs.Pinout -> FPinout
  | None -> FPnormal

module DependentKind = struct
  let to_string = function
    | DTthis -> SN.Typehints.this
    | DTexpr i ->
      let display_id = Reason.get_expr_display_id i in
      "<expr#" ^ string_of_int display_id ^ ">"

  let is_generic_dep_ty s = String_utils.is_substring "::" s
end

module ShapeFieldMap = struct
  include Nast.ShapeMap

  let map_and_rekey shape_map key_f value_f =
    let f_over_shape_field_type ({ sft_ty; _ } as shape_field_type) =
      { shape_field_type with sft_ty = value_f sft_ty }
    in
    Nast.ShapeMap.map_and_rekey shape_map key_f f_over_shape_field_type

  let map_env f env shape_map =
    let f_over_shape_field_type env _key ({ sft_ty; _ } as shape_field_type) =
      let (env, sft_ty) = f env sft_ty in
      (env, { shape_field_type with sft_ty })
    in
    Nast.ShapeMap.map_env f_over_shape_field_type env shape_map

  let map f shape_map = map_and_rekey shape_map (fun x -> x) f

  let iter f shape_map =
    let f_over_shape_field_type shape_map_key { sft_ty; _ } =
      f shape_map_key sft_ty
    in
    Nast.ShapeMap.iter f_over_shape_field_type shape_map

  let iter_values f = iter (fun _ -> f)
end

module ShapeFieldList = struct
  include Common.List

  let map_env env xs ~f =
    let f_over_shape_field_type env ({ sft_ty; _ } as shape_field_type) =
      let (env, sft_ty) = f env sft_ty in
      (env, { shape_field_type with sft_ty })
    in
    Common.List.map_env env xs ~f:f_over_shape_field_type
end

(*****************************************************************************)
(* Suggest mode *)
(*****************************************************************************)

(* Set to true when we are trying to infer the missing type hints. *)
let is_suggest_mode = ref false

(* Ordinal value for type constructor, for localized types *)
let ty_con_ordinal ty_ =
  match ty_ with
  | Tany _
  | Terr ->
    0
  | Toption t ->
    begin
      match get_node t with
      | Tnonnull -> 1
      | _ -> 4
    end
  | Tnonnull -> 2
  | Tdynamic -> 3
  | Tprim _ -> 5
  | Tfun _ -> 6
  | Ttuple _ -> 7
  | Tshape _ -> 8
  | Tvar _ -> 9
  | Tnewtype _ -> 10
  | Tgeneric _ -> 11
  | Tdependent _ -> 12
  | Tunion _ -> 13
  | Tintersection _ -> 14
  | Tobject -> 15
  | Tclass _ -> 16
  | Tvarray _ -> 20
  | Tdarray _ -> 21
  | Tvarray_or_darray _ -> 22
  | Tunapplied_alias _ -> 23
  | Taccess _ -> 24
  | Tvec_or_dict _ -> 25

(* Ordinal value for type constructor, for decl types *)
let decl_ty_con_ordinal ty_ =
  match ty_ with
  | Tany _
  | Terr ->
    0
  | Tthis -> 1
  | Tapply _ -> 2
  | Tgeneric _ -> 3
  | Taccess _ -> 4
  | Tdarray _ -> 6
  | Tvarray _ -> 7
  | Tvarray_or_darray _ -> 8
  | Tmixed -> 9
  | Tlike _ -> 10
  | Tnonnull -> 11
  | Tdynamic -> 12
  | Toption _ -> 13
  | Tprim _ -> 14
  | Tfun _ -> 15
  | Ttuple _ -> 16
  | Tshape _ -> 17
  | Tvar _ -> 19
  | Tunion _ -> 20
  | Tintersection _ -> 21
  | Tvec_or_dict _ -> 22

let reactivity_ordinal r =
  match r with
  | Nonreactive -> 0
  | CippGlobal -> 1
  | Pure _ -> 6
  | MaybeReactive _ -> 7
  | RxVar _ -> 8
  | Cipp _ -> 9
  | CippLocal _ -> 10

(* Compare two types syntactically, ignoring reason information and other
 * small differences that do not affect type inference behaviour. This
 * comparison function can be used to construct tree-based sets of types,
 * or to compare two types for "exact" equality.
 * Note that this function does *not* expand type variables, or type
 * aliases.
 * But if ty_compare ty1 ty2 = 0, then the types must not be distinguishable
 * by any typing rules.
 *)
let rec ty__compare ?(normalize_lists = false) ty_1 ty_2 =
  let rec ty__compare ty_1 ty_2 =
    match (ty_1, ty_2) with
    | (Tprim ty1, Tprim ty2) -> Aast_defs.compare_tprim ty1 ty2
    | (Toption ty, Toption ty2)
    | (Tvarray ty, Tvarray ty2) ->
      ty_compare ty ty2
    | (Tdarray (tk, tv), Tdarray (tk2, tv2))
    | (Tvarray_or_darray (tk, tv), Tvarray_or_darray (tk2, tv2)) ->
      begin
        match ty_compare tk tk2 with
        | 0 -> ty_compare tv tv2
        | n -> n
      end
    | (Tfun fty, Tfun fty2) -> tfun_compare fty fty2
    | (Tunion tyl1, Tunion tyl2)
    | (Tintersection tyl1, Tintersection tyl2)
    | (Ttuple tyl1, Ttuple tyl2) ->
      tyl_compare ~sort:normalize_lists ~normalize_lists tyl1 tyl2
    | (Tgeneric (n1, args1), Tgeneric (n2, args2)) ->
      begin
        match String.compare n1 n2 with
        | 0 -> tyl_compare ~sort:false ~normalize_lists args1 args2
        | n -> n
      end
    | (Tnewtype (id, tyl, cstr1), Tnewtype (id2, tyl2, cstr2)) ->
      begin
        match String.compare id id2 with
        | 0 ->
          (match tyl_compare ~sort:false tyl tyl2 with
          | 0 -> ty_compare cstr1 cstr2
          | n -> n)
        | n -> n
      end
    | (Tdependent (d1, cstr1), Tdependent (d2, cstr2)) ->
      begin
        match compare_dependent_type d1 d2 with
        | 0 -> ty_compare cstr1 cstr2
        | n -> n
      end
    (* An instance of a class or interface, ty list are the arguments *)
    | (Tclass (id, exact, tyl), Tclass (id2, exact2, tyl2)) ->
      begin
        match String.compare (snd id) (snd id2) with
        | 0 ->
          begin
            match tyl_compare ~sort:false tyl tyl2 with
            | 0 -> compare_exact exact exact2
            | n -> n
          end
        | n -> n
      end
    | (Tshape (shape_kind1, fields1), Tshape (shape_kind2, fields2)) ->
      begin
        match compare_shape_kind shape_kind1 shape_kind2 with
        | 0 ->
          List.compare
            (fun (k1, v1) (k2, v2) ->
              match Ast_defs.ShapeField.compare k1 k2 with
              | 0 -> shape_field_type_compare v1 v2
              | n -> n)
            (Nast.ShapeMap.elements fields1)
            (Nast.ShapeMap.elements fields2)
        | n -> n
      end
    | (Tvar v1, Tvar v2) -> compare v1 v2
    | (Tunapplied_alias n1, Tunapplied_alias n2) -> String.compare n1 n2
    | (Taccess (ty1, id1), Taccess (ty2, id2)) ->
      begin
        match ty_compare ty1 ty2 with
        | 0 -> String.compare (snd id1) (snd id2)
        | n -> n
      end
    | (Tnonnull, Tnonnull) -> 0
    | (Tdynamic, Tdynamic) -> 0
    | (Tobject, Tobject) -> 0
    | (Terr, Terr) -> 0
    | ( ( Tprim _ | Toption _ | Tvarray _ | Tdarray _ | Tvarray_or_darray _
        | Tvec_or_dict _ | Tfun _ | Tintersection _ | Tunion _ | Ttuple _
        | Tgeneric _ | Tnewtype _ | Tdependent _ | Tclass _ | Tshape _ | Tvar _
        | Tunapplied_alias _ | Tnonnull | Tdynamic | Terr | Tobject | Taccess _
        | Tany _ ),
        _ )
    | ( _,
        ( Tprim _ | Toption _ | Tvarray _ | Tdarray _ | Tvarray_or_darray _
        | Tvec_or_dict _ | Tfun _ | Tintersection _ | Tunion _ | Ttuple _
        | Tgeneric _ | Tnewtype _ | Tdependent _ | Tclass _ | Tshape _ | Tvar _
        | Tunapplied_alias _ | Tnonnull | Tdynamic | Terr | Tobject | Taccess _
        | Tany _ ) ) ->
      ty_con_ordinal ty_1 - ty_con_ordinal ty_2
  and shape_field_type_compare sft1 sft2 =
    let { sft_ty = ty1; sft_optional = optional1 } = sft1 in
    let { sft_ty = ty2; sft_optional = optional2 } = sft2 in
    match ty_compare ty1 ty2 with
    | 0 -> Bool.compare optional1 optional2
    | n -> n
  and user_attribute_compare ua1 ua2 =
    let { ua_name = name1; ua_classname_params = classname_params1 } = ua1 in
    let { ua_name = name2; ua_classname_params = classname_params2 } = ua2 in
    match String.compare (snd name1) (snd name2) with
    | 0 -> List.compare String.compare classname_params1 classname_params2
    | n -> n
  and user_attributes_compare ual1 ual2 =
    List.compare user_attribute_compare ual1 ual2
  and tparam_compare tp1 tp2 =
    let {
      (* Type parameters on functions are always marked invariant *)
      tp_variance = _;
      tp_name = name1;
      tp_tparams = tparams1;
      tp_constraints = constraints1;
      tp_reified = reified1;
      tp_user_attributes = user_attributes1;
    } =
      tp1
    in
    let {
      tp_variance = _;
      tp_name = name2;
      tp_tparams = tparams2;
      tp_constraints = constraints2;
      tp_reified = reified2;
      tp_user_attributes = user_attributes2;
    } =
      tp2
    in
    match String.compare (snd name1) (snd name2) with
    | 0 ->
      begin
        match tparams_compare tparams1 tparams2 with
        | 0 ->
          begin
            match constraints_compare constraints1 constraints2 with
            | 0 ->
              begin
                match
                  user_attributes_compare user_attributes1 user_attributes2
                with
                | 0 -> Aast_defs.compare_reify_kind reified1 reified2
                | n -> n
              end
            | n -> n
          end
        | n -> n
      end
    | n -> n
  and tparams_compare tpl1 tpl2 = List.compare tparam_compare tpl1 tpl2
  and constraints_compare cl1 cl2 = List.compare constraint_compare cl1 cl2
  and constraint_compare (ck1, ty1) (ck2, ty2) =
    match Ast_defs.compare_constraint_kind ck1 ck2 with
    | 0 -> ty_compare ty1 ty2
    | n -> n
  and where_constraint_compare (ty1a, ck1, ty1b) (ty2a, ck2, ty2b) =
    match Ast_defs.compare_constraint_kind ck1 ck2 with
    | 0 ->
      begin
        match ty_compare ty1a ty2a with
        | 0 -> ty_compare ty1b ty2b
        | n -> n
      end
    | n -> n
  and where_constraints_compare cl1 cl2 =
    List.compare where_constraint_compare cl1 cl2
  (* We match every field rather than using field selection syntax. This guards against future additions to function type elements *)
  and tfun_compare fty1 fty2 =
    let {
      ft_ret = ret1;
      ft_params = params1;
      ft_arity = arity1;
      ft_reactive = reactive1;
      ft_flags = flags1;
      ft_implicit_params = implicit_params1;
      ft_ifc_decl = ifc_decl1;
      ft_tparams = tparams1;
      ft_where_constraints = where_constraints1;
    } =
      fty1
    in
    let {
      ft_ret = ret2;
      ft_params = params2;
      ft_arity = arity2;
      ft_reactive = reactive2;
      ft_flags = flags2;
      ft_implicit_params = implicit_params2;
      ft_ifc_decl = ifc_decl2;
      ft_tparams = tparams2;
      ft_where_constraints = where_constraints2;
    } =
      fty2
    in
    match possibly_enforced_ty_compare ret1 ret2 with
    | 0 ->
      begin
        match ft_params_compare params1 params2 with
        | 0 ->
          (* Explicit polymorphic equality. Need to write equality on
           * locl_ty by hand if we want to make a specialized one
           *)
          begin
            match ft_arity_compare arity1 arity2 with
            | 0 ->
              begin
                match tparams_compare tparams1 tparams2 with
                | 0 ->
                  begin
                    match
                      where_constraints_compare
                        where_constraints1
                        where_constraints2
                    with
                    | 0 ->
                      begin
                        match Int.compare flags1 flags2 with
                        | 0 ->
                          let { capability = capability1 } = implicit_params1 in
                          let { capability = capability2 } = implicit_params2 in
                          begin
                            match
                              capability_compare capability1 capability2
                            with
                            | 0 ->
                              begin
                                match
                                  compare_ifc_fun_decl ifc_decl1 ifc_decl2
                                with
                                | 0 -> reactivity_compare reactive1 reactive2
                                | n -> n
                              end
                            | n -> n
                          end
                        | n -> n
                      end
                    | n -> n
                  end
                | n -> n
              end
            | n -> n
          end
        | n -> n
      end
    | n -> n
  and ft_arity_compare a1 a2 =
    match (a1, a2) with
    | (Fstandard, Fstandard) -> 0
    | (Fstandard, Fvariadic _) -> -1
    | (Fvariadic _, Fstandard) -> 1
    | (Fvariadic p1, Fvariadic p2) -> ft_param_compare ~normalize_lists p1 p2
  and capability_compare cap1 cap2 =
    match (cap1, cap2) with
    | (CapDefaults _, CapDefaults _) -> 0
    | (CapDefaults _, CapTy _) -> -1
    | (CapTy _, CapDefaults _) -> 1
    | (CapTy ty1, CapTy ty2) -> ty_compare ty1 ty2
  and reactivity_compare r1 r2 =
    match (r1, r2) with
    | (Nonreactive, Nonreactive)
    | (CippGlobal, CippGlobal) ->
      0
    | (Pure opt_ty1, Pure opt_ty2) ->
      (* TODO T82455489: proper decl compare. Poly.compare will be position sensitive *)
      Option.compare Poly.compare opt_ty1 opt_ty2
    | (MaybeReactive r1, MaybeReactive r2) -> reactivity_compare r1 r2
    | (RxVar opt_r1, RxVar opt_r2) ->
      Option.compare reactivity_compare opt_r1 opt_r2
    | (Cipp opt_s1, Cipp opt_s2)
    | (CippLocal opt_s1, CippLocal opt_s2) ->
      Option.compare String.compare opt_s1 opt_s2
    | ( ( Nonreactive | CippGlobal | Pure _ | MaybeReactive _ | RxVar _ | Cipp _
        | CippLocal _ ),
        ( Nonreactive | CippGlobal | Pure _ | MaybeReactive _ | RxVar _ | Cipp _
        | CippLocal _ ) ) ->
      reactivity_ordinal r1 - reactivity_ordinal r2
  and ty_compare ty1 ty2 = ty__compare (get_node ty1) (get_node ty2) in
  ty__compare ty_1 ty_2

and ty_compare ?(normalize_lists = false) ty1 ty2 =
  ty__compare ~normalize_lists (get_node ty1) (get_node ty2)

and tyl_compare ~sort ?(normalize_lists = false) tyl1 tyl2 =
  let (tyl1, tyl2) =
    if sort then
      (List.sort ~compare:ty_compare tyl1, List.sort ~compare:ty_compare tyl2)
    else
      (tyl1, tyl2)
  in
  List.compare (ty_compare ~normalize_lists) tyl1 tyl2

and possibly_enforced_ty_compare ?(normalize_lists = false) ety1 ety2 =
  match ty_compare ~normalize_lists ety1.et_type ety2.et_type with
  | 0 -> Bool.compare ety1.et_enforced ety2.et_enforced
  | n -> n

and ft_param_compare ?(normalize_lists = false) param1 param2 =
  match
    possibly_enforced_ty_compare ~normalize_lists param1.fp_type param2.fp_type
  with
  | 0 -> Int.compare param1.fp_flags param2.fp_flags
  | n -> n

and ft_params_compare ?(normalize_lists = false) params1 params2 =
  List.compare (ft_param_compare ~normalize_lists) params1 params2

let tyl_equal tyl1 tyl2 = Int.equal 0 @@ tyl_compare ~sort:false tyl1 tyl2

let class_id_con_ordinal cid =
  match cid with
  | Aast.CIparent -> 0
  | Aast.CIself -> 1
  | Aast.CIstatic -> 2
  | Aast.CIexpr _ -> 3
  | Aast.CI _ -> 4

let class_id_compare cid1 cid2 =
  match (cid1, cid2) with
  | (Aast.CIexpr _e1, Aast.CIexpr _e2) -> 0
  | (Aast.CI (_, id1), Aast.CI (_, id2)) -> String.compare id1 id2
  | _ -> class_id_con_ordinal cid2 - class_id_con_ordinal cid1

let class_id_equal cid1 cid2 = Int.equal (class_id_compare cid1 cid2) 0

let has_member_compare ~normalize_lists hm1 hm2 =
  let ty_compare = ty_compare ~normalize_lists in
  let {
    hm_name = (_, m1);
    hm_type = ty1;
    hm_class_id = cid1;
    hm_explicit_targs = targs1;
  } =
    hm1
  in
  let {
    hm_name = (_, m2);
    hm_type = ty2;
    hm_class_id = cid2;
    hm_explicit_targs = targs2;
  } =
    hm2
  in
  let targ_compare (_, (_, hint1)) (_, (_, hint2)) =
    Aast_defs.compare_hint_ hint1 hint2
  in
  match String.compare m1 m2 with
  | 0 ->
    (match ty_compare ty1 ty2 with
    | 0 ->
      (match class_id_compare cid1 cid2 with
      | 0 -> Option.compare (List.compare targ_compare) targs1 targs2
      | comp -> comp)
    | comp -> comp)
  | comp -> comp

let destructure_compare ~normalize_lists d1 d2 =
  let {
    d_required = tyl1;
    d_optional = tyl_opt1;
    d_variadic = ty_opt1;
    d_kind = e1;
  } =
    d1
  in
  let {
    d_required = tyl2;
    d_optional = tyl_opt2;
    d_variadic = ty_opt2;
    d_kind = e2;
  } =
    d2
  in
  match tyl_compare ~normalize_lists ~sort:false tyl1 tyl2 with
  | 0 ->
    (match tyl_compare ~normalize_lists ~sort:false tyl_opt1 tyl_opt2 with
    | 0 ->
      (match Option.compare ty_compare ty_opt1 ty_opt2 with
      | 0 -> compare_destructure_kind e1 e2
      | comp -> comp)
    | comp -> comp)
  | comp -> comp

let constraint_ty_con_ordinal cty =
  match cty with
  | Thas_member _ -> 0
  | Tdestructure _ -> 1
  | TCunion _ -> 2
  | TCintersection _ -> 3

let rec constraint_ty_compare ?(normalize_lists = false) ty1 ty2 =
  let (_, ty1) = deref_constraint_type ty1 in
  let (_, ty2) = deref_constraint_type ty2 in
  match (ty1, ty2) with
  | (Thas_member hm1, Thas_member hm2) ->
    has_member_compare ~normalize_lists hm1 hm2
  | (Tdestructure d1, Tdestructure d2) ->
    destructure_compare ~normalize_lists d1 d2
  | (TCunion (lty1, cty1), TCunion (lty2, cty2))
  | (TCintersection (lty1, cty1), TCintersection (lty2, cty2)) ->
    let comp1 = ty_compare ~normalize_lists lty1 lty2 in
    if not @@ Int.equal comp1 0 then
      comp1
    else
      constraint_ty_compare ~normalize_lists cty1 cty2
  | (_, (Thas_member _ | Tdestructure _ | TCunion _ | TCintersection _)) ->
    constraint_ty_con_ordinal ty2 - constraint_ty_con_ordinal ty1

let constraint_ty_equal ?(normalize_lists = false) ty1 ty2 =
  Int.equal (constraint_ty_compare ~normalize_lists ty1 ty2) 0

let ty_equal ?(normalize_lists = false) ty1 ty2 =
  Int.equal 0 (ty_compare ~normalize_lists ty1 ty2)

let equal_internal_type ty1 ty2 =
  match (ty1, ty2) with
  | (LoclType ty1, LoclType ty2) -> ty_equal ~normalize_lists:true ty1 ty2
  | (ConstraintType ty1, ConstraintType ty2) ->
    constraint_ty_equal ~normalize_lists:true ty1 ty2
  | (_, (LoclType _ | ConstraintType _)) -> false

let equal_locl_ty ty1 ty2 = ty_equal ty1 ty2

let equal_locl_ty_ ty_1 ty_2 = Int.equal 0 (ty__compare ty_1 ty_2)

let equal_locl_fun_arity ft1 ft2 =
  match (ft1.ft_arity, ft2.ft_arity) with
  | (Fstandard, Fstandard) ->
    Int.equal (List.length ft1.ft_params) (List.length ft2.ft_params)
  | (Fvariadic param1, Fvariadic param2) ->
    Int.equal 0 (ft_params_compare [param1] [param2])
  | (Fstandard, Fvariadic _)
  | (Fvariadic _, Fstandard) ->
    false

let is_type_no_return = equal_locl_ty_ (Tprim Aast.Tnoreturn)

let make_function_type_rxvar param_ty =
  match deref param_ty with
  | (r, Tfun tfun) -> mk (r, Tfun { tfun with ft_reactive = RxVar None })
  | (r, Toption t) ->
    begin
      match deref t with
      | (r1, Tfun tfun) ->
        mk (r, Toption (mk (r1, Tfun { tfun with ft_reactive = RxVar None })))
      | _ -> param_ty
    end
  | _ -> param_ty

let rec equal_decl_ty_ ty_1 ty_2 =
  match (ty_1, ty_2) with
  | (Tany _, Tany _) -> true
  | (Terr, Terr) -> true
  | (Tthis, Tthis) -> true
  | (Tmixed, Tmixed) -> true
  | (Tnonnull, Tnonnull) -> true
  | (Tdynamic, Tdynamic) -> true
  | (Tapply ((_, s1), tyl1), Tapply ((_, s2), tyl2)) ->
    String.equal s1 s2 && equal_decl_tyl tyl1 tyl2
  | (Tgeneric (s1, argl1), Tgeneric (s2, argl2)) ->
    String.equal s1 s2 && equal_decl_tyl argl1 argl2
  | (Taccess (ty1, (_, s1)), Taccess (ty2, (_, s2))) ->
    equal_decl_ty ty1 ty2 && String.equal s1 s2
  | (Tdarray (tk1, tv1), Tdarray (tk2, tv2)) ->
    equal_decl_ty tk1 tk2 && equal_decl_ty tv1 tv2
  | (Tvarray ty1, Tvarray ty2) -> equal_decl_ty ty1 ty2
  | (Tvarray_or_darray (tk1, tv1), Tvarray_or_darray (tk2, tv2)) ->
    equal_decl_ty tk1 tk2 && equal_decl_ty tv1 tv2
  | (Tvec_or_dict (tk1, tv1), Tvec_or_dict (tk2, tv2)) ->
    equal_decl_ty tk1 tk2 && equal_decl_ty tv1 tv2
  | (Tlike ty1, Tlike ty2) -> equal_decl_ty ty1 ty2
  | (Tprim ty1, Tprim ty2) -> Aast.equal_tprim ty1 ty2
  | (Toption ty, Toption ty2) -> equal_decl_ty ty ty2
  | (Tfun fty1, Tfun fty2) -> equal_decl_fun_type fty1 fty2
  | (Tunion tyl1, Tunion tyl2)
  | (Tintersection tyl1, Tintersection tyl2)
  | (Ttuple tyl1, Ttuple tyl2) ->
    equal_decl_tyl tyl1 tyl2
  | (Tshape (shape_kind1, fields1), Tshape (shape_kind2, fields2)) ->
    equal_shape_kind shape_kind1 shape_kind2
    && List.equal
         (fun (k1, v1) (k2, v2) ->
           Ast_defs.ShapeField.equal k1 k2 && equal_shape_field_type v1 v2)
         (Nast.ShapeMap.elements fields1)
         (Nast.ShapeMap.elements fields2)
  | (Tvar v1, Tvar v2) -> Ident.equal v1 v2
  | (Tany _, _)
  | (Terr, _)
  | (Tthis, _)
  | (Tapply _, _)
  | (Tgeneric _, _)
  | (Taccess _, _)
  | (Tdarray _, _)
  | (Tvarray _, _)
  | (Tvarray_or_darray _, _)
  | (Tvec_or_dict _, _)
  | (Tmixed, _)
  | (Tlike _, _)
  | (Tnonnull, _)
  | (Tdynamic, _)
  | (Toption _, _)
  | (Tprim _, _)
  | (Tfun _, _)
  | (Ttuple _, _)
  | (Tshape _, _)
  | (Tvar _, _)
  | (Tunion _, _)
  | (Tintersection _, _) ->
    false

and equal_decl_ty ty1 ty2 = equal_decl_ty_ (get_node ty1) (get_node ty2)

and equal_shape_field_type sft1 sft2 =
  equal_decl_ty sft1.sft_ty sft2.sft_ty
  && Bool.equal sft1.sft_optional sft2.sft_optional

and equal_decl_fun_arity ft1 ft2 =
  match (ft1.ft_arity, ft2.ft_arity) with
  | (Fstandard, Fstandard) ->
    Int.equal (List.length ft1.ft_params) (List.length ft2.ft_params)
  | (Fvariadic param1, Fvariadic param2) ->
    equal_decl_ft_params [param1] [param2]
  | (Fstandard, Fvariadic _)
  | (Fvariadic _, Fstandard) ->
    false

and equal_decl_fun_type fty1 fty2 =
  equal_decl_possibly_enforced_ty fty1.ft_ret fty2.ft_ret
  && equal_decl_ft_params fty1.ft_params fty2.ft_params
  && equal_decl_ft_implicit_params
       fty1.ft_implicit_params
       fty2.ft_implicit_params
  && equal_decl_fun_arity fty1 fty2
  && equal_reactivity fty1.ft_reactive fty2.ft_reactive
  && Int.equal fty1.ft_flags fty2.ft_flags

and equal_reactivity r1 r2 =
  match (r1, r2) with
  | (Nonreactive, Nonreactive) -> true
  | (Pure ty1, Pure ty2) -> Option.equal equal_decl_ty ty1 ty2
  | (MaybeReactive r1, MaybeReactive r2) -> equal_reactivity r1 r2
  | (RxVar r1, RxVar r2) -> Option.equal equal_reactivity r1 r2
  | (Cipp s1, Cipp s2) -> Option.equal String.equal s1 s2
  | (CippLocal s1, CippLocal s2) -> Option.equal String.equal s1 s2
  | (CippGlobal, CippGlobal) -> true
  | _ -> false

and any_reactive r =
  match r with
  | Pure _
  | MaybeReactive _
  | RxVar _ ->
    true
  | Nonreactive
  | Cipp _
  | CippLocal _
  | CippGlobal ->
    false

and non_public_ifc ifc =
  match ifc with
  | FDPolicied (Some "PUBLIC") -> false
  | _ -> true

and equal_param_rx_annotation pa1 pa2 =
  match (pa1, pa2) with
  | (Param_rx_var, Param_rx_var) -> true
  | (Param_rx_if_impl ty1, Param_rx_if_impl ty2) -> equal_decl_ty ty1 ty2
  | (Param_rx_var, Param_rx_if_impl _)
  | (Param_rx_if_impl _, Param_rx_var) ->
    false

and equal_decl_tyl tyl1 tyl2 = List.equal equal_decl_ty tyl1 tyl2

and equal_decl_possibly_enforced_ty ety1 ety2 =
  equal_decl_ty ety1.et_type ety2.et_type
  && Bool.equal ety1.et_enforced ety2.et_enforced

and equal_decl_fun_param param1 param2 =
  equal_decl_possibly_enforced_ty param1.fp_type param2.fp_type
  && Int.equal param1.fp_flags param2.fp_flags

and equal_decl_ft_params params1 params2 =
  List.equal equal_decl_fun_param params1 params2

and equal_decl_ft_implicit_params { capability = cap1 } { capability = cap2 } =
  (* TODO(coeffects): could rework this so that implicit defaults and explicit
   * [defaults] are considered equal *)
  match (cap1, cap2) with
  | (CapDefaults p1, CapDefaults p2) -> Pos.equal p1 p2
  | (CapTy c1, CapTy c2) -> equal_decl_ty c1 c2
  | (CapDefaults _, CapTy _)
  | (CapTy _, CapDefaults _) ->
    false

let equal_typeconst_abstract_kind ak1 ak2 =
  match (ak1, ak2) with
  | (TCAbstract ty1, TCAbstract ty2) -> Option.equal equal_decl_ty ty1 ty2
  | (TCPartiallyAbstract, TCPartiallyAbstract) -> true
  | (TCConcrete, TCConcrete) -> true
  | (TCAbstract _, _)
  | (TCPartiallyAbstract, _)
  | (TCConcrete, _) ->
    false

let equal_enum_type et1 et2 =
  equal_decl_ty et1.te_base et2.te_base
  && Option.equal equal_decl_ty et1.te_constraint et2.te_constraint

let equal_decl_where_constraint c1 c2 =
  let (tya1, ck1, tyb1) = c1 in
  let (tya2, ck2, tyb2) = c2 in
  equal_decl_ty tya1 tya2
  && Ast_defs.equal_constraint_kind ck1 ck2
  && equal_decl_ty tyb1 tyb2

let equal_decl_tparam tp1 tp2 =
  Ast_defs.equal_variance tp1.tp_variance tp2.tp_variance
  && Ast_defs.equal_id tp1.tp_name tp2.tp_name
  && List.equal
       (Tuple.T2.equal ~eq1:Ast_defs.equal_constraint_kind ~eq2:equal_decl_ty)
       tp1.tp_constraints
       tp2.tp_constraints
  && Aast.equal_reify_kind tp1.tp_reified tp2.tp_reified
  && List.equal
       equal_user_attribute
       tp1.tp_user_attributes
       tp2.tp_user_attributes

let equal_typedef_type tt1 tt2 =
  Pos.equal tt1.td_pos tt2.td_pos
  && Aast.equal_typedef_visibility tt1.td_vis tt2.td_vis
  && List.equal equal_decl_tparam tt1.td_tparams tt2.td_tparams
  && Option.equal equal_decl_ty tt1.td_constraint tt2.td_constraint
  && equal_decl_ty tt1.td_type tt2.td_type

let equal_fun_elt fe1 fe2 =
  Option.equal String.equal fe1.fe_deprecated fe2.fe_deprecated
  && equal_decl_ty fe1.fe_type fe2.fe_type
  && Pos.equal fe1.fe_pos fe2.fe_pos

let equal_const_decl cd1 cd2 =
  Pos.equal cd1.cd_pos cd2.cd_pos && equal_decl_ty cd1.cd_type cd2.cd_type

let get_ce_abstract ce = is_set ce_flags_abstract ce.ce_flags

let get_ce_final ce = is_set ce_flags_final ce.ce_flags

let get_ce_override ce = is_set ce_flags_override ce.ce_flags

let get_ce_lsb ce = is_set ce_flags_lsb ce.ce_flags

let get_ce_synthesized ce = is_set ce_flags_synthesized ce.ce_flags

let get_ce_const ce = is_set ce_flags_const ce.ce_flags

let get_ce_lateinit ce = is_set ce_flags_lateinit ce.ce_flags

let get_ce_readonly_prop ce = is_set ce_flags_readonly_prop ce.ce_flags

let get_ce_dynamicallycallable ce =
  is_set ce_flags_dynamicallycallable ce.ce_flags

let xhp_attr_to_ce_flags xa =
  match xa with
  | None -> 0x0
  | Some { xa_tag; xa_has_default } ->
    Int.bit_or
      ( if xa_has_default then
        ce_flags_xa_has_default
      else
        0x0 )
    @@
    (match xa_tag with
    | None -> ce_flags_xa_tag_none
    | Some Required -> ce_flags_xa_tag_required
    | Some Lateinit -> ce_flags_xa_tag_lateinit)

let flags_to_xhp_attr flags =
  let tag_flags = Int.bit_and ce_flags_xa_tag_mask flags in
  if Int.equal tag_flags 0 then
    None
  else
    Some
      {
        xa_has_default = is_set ce_flags_xa_has_default flags;
        xa_tag =
          ( if Int.equal tag_flags ce_flags_xa_tag_none then
            None
          else if Int.equal tag_flags ce_flags_xa_tag_required then
            Some Required
          else
            Some Lateinit );
      }

let get_ce_xhp_attr ce = flags_to_xhp_attr ce.ce_flags

let make_ce_flags
    ~xhp_attr
    ~abstract
    ~final
    ~override
    ~lsb
    ~synthesized
    ~const
    ~lateinit
    ~dynamicallycallable
    ~readonly_prop =
  let flags = 0 in
  let flags = set_bit ce_flags_abstract abstract flags in
  let flags = set_bit ce_flags_final final flags in
  let flags = set_bit ce_flags_override override flags in
  let flags = set_bit ce_flags_lsb lsb flags in
  let flags = set_bit ce_flags_synthesized synthesized flags in
  let flags = set_bit ce_flags_const const flags in
  let flags = set_bit ce_flags_lateinit lateinit flags in
  let flags = set_bit ce_flags_dynamicallycallable dynamicallycallable flags in
  let flags = Int.bit_or flags (xhp_attr_to_ce_flags xhp_attr) in
  let flags = set_bit ce_flags_readonly_prop readonly_prop flags in
  flags

(** Tunapplied_alias is a locl phase constructor that always stands for a higher-kinded type.
  We use this function in cases where Tunapplied_alias appears in a context where we expect
  a fully applied type, rather than a type constructor. Seeing Tunapplied_alias in such a context
  always indicates a kinding error, which means that during localization, we should have
  created Terr rather than Tunapplied_alias. Hence, this is an *internal* error, because
  something went wrong during localization. Kind mismatches in code are reported to users
  elsewhere. *)
let error_Tunapplied_alias_in_illegal_context () =
  failwith "Found Tunapplied_alias in a context where it must not occur"

module Attributes = struct
  let mem x xs =
    List.exists xs (fun { ua_name; _ } -> String.equal x (snd ua_name))

  let find x xs =
    List.find xs (fun { ua_name; _ } -> String.equal x (snd ua_name))
end
