// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the "hack" directory of this source tree.
//
// @generated SignedSource<<a57f04669155ecb9e9e22a168f04db97>>
//
// To regenerate this file, run:
//   hphp/hack/src/oxidized_by_ref/regen.sh

use serde::Serialize;

#[allow(unused_imports)]
use crate::*;

pub use typing_defs::*;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ShallowClassConst<'a> {
    pub abstract_: bool,
    pub expr: Option<nast::Expr<'a>>,
    pub name: aast::Sid<'a>,
    pub type_: Ty<'a>,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ShallowTypeconst<'a> {
    pub abstract_: TypeconstAbstractKind<'a>,
    pub constraint: Option<Ty<'a>>,
    pub name: aast::Sid<'a>,
    pub type_: Option<Ty<'a>>,
    pub enforceable: (pos::Pos<'a>, bool),
    pub reifiable: Option<pos::Pos<'a>>,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ShallowPuMember<'a> {
    pub atom: aast::Sid<'a>,
    pub types: &'a [(aast::Sid<'a>, Ty<'a>)],
    pub exprs: &'a [aast::Sid<'a>],
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ShallowPuEnum<'a> {
    pub name: aast::Sid<'a>,
    pub is_final: bool,
    pub case_types: &'a [(aast::Sid<'a>, oxidized::aast::ReifyKind)],
    pub case_values: &'a [(aast::Sid<'a>, Ty<'a>)],
    pub members: &'a [ShallowPuMember<'a>],
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ShallowProp<'a> {
    pub const_: bool,
    pub xhp_attr: Option<oxidized::shallow_decl_defs::XhpAttr>,
    pub lateinit: bool,
    pub lsb: bool,
    pub name: aast::Sid<'a>,
    pub needs_init: bool,
    pub type_: Option<Ty<'a>>,
    pub abstract_: bool,
    pub visibility: oxidized::aast::Visibility,
    pub fixme_codes: i_set::ISet<'a>,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ShallowMethod<'a> {
    pub abstract_: bool,
    pub final_: bool,
    pub memoizelsb: bool,
    pub name: aast::Sid<'a>,
    pub override_: bool,
    pub dynamicallycallable: bool,
    pub reactivity: Option<decl_defs::MethodReactivity<'a>>,
    pub type_: Ty<'a>,
    pub visibility: oxidized::aast::Visibility,
    pub fixme_codes: i_set::ISet<'a>,
    pub deprecated: Option<&'a str>,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ShallowMethodRedeclaration<'a> {
    pub abstract_: bool,
    pub final_: bool,
    pub static_: bool,
    pub name: aast::Sid<'a>,
    pub type_: Ty<'a>,
    pub visibility: oxidized::aast::Visibility,
    pub trait_: aast::Hint<'a>,
    pub method: aast::Pstring<'a>,
    pub fixme_codes: i_set::ISet<'a>,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ShallowClass<'a> {
    pub mode: oxidized::file_info::Mode,
    pub final_: bool,
    pub is_xhp: bool,
    pub has_xhp_keyword: bool,
    pub kind: oxidized::ast_defs::ClassKind,
    pub name: aast::Sid<'a>,
    pub tparams: &'a [Tparam<'a>],
    pub where_constraints: &'a [WhereConstraint<'a>],
    pub extends: &'a [Ty<'a>],
    pub uses: &'a [Ty<'a>],
    pub method_redeclarations: &'a [ShallowMethodRedeclaration<'a>],
    pub xhp_attr_uses: &'a [Ty<'a>],
    pub req_extends: &'a [Ty<'a>],
    pub req_implements: &'a [Ty<'a>],
    pub implements: &'a [Ty<'a>],
    pub consts: &'a [ShallowClassConst<'a>],
    pub typeconsts: &'a [ShallowTypeconst<'a>],
    pub pu_enums: &'a [ShallowPuEnum<'a>],
    pub props: &'a [ShallowProp<'a>],
    pub sprops: &'a [ShallowProp<'a>],
    pub constructor: Option<ShallowMethod<'a>>,
    pub static_methods: &'a [ShallowMethod<'a>],
    pub methods: &'a [ShallowMethod<'a>],
    pub user_attributes: &'a [nast::UserAttribute<'a>],
    pub enum_type: Option<EnumType<'a>>,
    pub decl_errors: errors::Errors<'a>,
}