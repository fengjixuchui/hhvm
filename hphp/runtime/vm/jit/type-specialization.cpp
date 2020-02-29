/*
   +----------------------------------------------------------------------+
   | HipHop for PHP                                                       |
   +----------------------------------------------------------------------+
   | Copyright (c) 2010-present Facebook, Inc. (http://www.facebook.com)  |
   +----------------------------------------------------------------------+
   | This source file is subject to version 3.01 of the PHP license,      |
   | that is bundled with this package in the file LICENSE, and is        |
   | available through the world-wide-web at the following url:           |
   | http://www.php.net/license/3_01.txt                                  |
   | If you did not receive a copy of the PHP license and are unable to   |
   | obtain it through the world-wide-web, please send a note to          |
   | license@php.net so we can mail you a copy immediately.               |
   +----------------------------------------------------------------------+
*/

#include "hphp/runtime/vm/jit/type-specialization.h"

#include "hphp/runtime/base/repo-auth-type-array.h"
#include "hphp/runtime/base/string-data.h"
#include "hphp/runtime/vm/class.h"

namespace HPHP { namespace jit {
///////////////////////////////////////////////////////////////////////////////
// ArraySpec.

bool ArraySpec::operator<=(const ArraySpec& rhs) const {
  assertx(checkInvariants());
  assertx(rhs.checkInvariants());
  auto const& lhs = *this;

  if (lhs == Bottom() || rhs == Top()) return true;
  if (lhs == Top() || rhs == Bottom()) return false;

  // It's possible to subtype RAT::Array types, but it's potentially O(n), so
  // we just don't do it. It's okay for <= to return false negative results.
  if ((rhs.m_sort & HasKind) &&
      !((lhs.m_sort & HasKind) && lhs.m_kind == rhs.m_kind)) {
    return false;
  }
  if ((rhs.m_sort & HasType) &&
      !((lhs.m_sort & HasType) && lhs.m_ptr == rhs.m_ptr)) {
    return false;
  }
  if (rhs.vanilla() && !lhs.vanilla()) {
    return false;
  }
  return true;
}

ArraySpec ArraySpec::operator|(const ArraySpec& rhs) const {
  assertx(checkInvariants());
  assertx(rhs.checkInvariants());
  auto const& lhs = *this;

  if (lhs <= rhs) return rhs;
  if (rhs <= lhs) return lhs;

  // Operate on the raw fields; kind() masks the kind based on the vanilla bit,
  // but we still want to propagate the value in case we later get that bit.
  //
  // Note that each bit in m_sort represents some fact we know about the type.
  // To union the types, we must intersect (and thus lose) some of these facts.
  auto result = lhs;
  result.m_sort &= rhs.m_sort;
  if (lhs.m_kind != rhs.m_kind) {
    result.m_sort &= ~HasKind;
    result.m_kind = ArrayData::ArrayKind{};
  }
  if (lhs.m_ptr != rhs.m_ptr) {
    result.m_sort &= ~HasType;
    result.m_ptr = 0;
  }
  assertx(result.checkInvariants());
  return result;
}

ArraySpec ArraySpec::operator&(const ArraySpec& rhs) const {
  assertx(checkInvariants());
  assertx(rhs.checkInvariants());
  auto const& lhs = *this;

  if (lhs <= rhs) return lhs;
  if (rhs <= lhs) return rhs;

  // Operate on the raw fields; kind() masks the kind based on the vanilla bit,
  // but we still want to propagate the value in case we later get that bit.
  //
  // Note that each bit in m_sort represents some fact we know about the type.
  // To intersect the types, we may union (and thus gain) some of these facts.
  auto result = lhs;
  result.m_sort |= rhs.m_sort;

  // If both types have a kind and they differ, the intersection must be empty.
  if (rhs.m_sort & HasKind) {
    if ((lhs.m_sort & HasKind) && lhs.m_kind != rhs.m_kind) {
      return Bottom();
    }
    result.m_kind = rhs.m_kind;
  }

  // If both types have an RAT and they differ, then we must drop this field
  // from the specialization (because it's expensive to intersect RATs).
  if (rhs.m_sort & HasType) {
    if ((lhs.m_sort & HasType) && lhs.m_ptr != rhs.m_ptr) {
      result.m_sort &= ~HasType;
      result.m_ptr = 0;
    } else {
      result.m_ptr = rhs.m_ptr;
    }
  }

  result.checkInvariants();
  return result;
}

std::string ArraySpec::toString() const {
  std::string result;
  auto const init = (m_sort & IsVanilla) ? "=" : "={";
  if (m_sort & HasKind) {
    auto const kind = ArrayData::ArrayKind(m_kind);
    result += folly::to<std::string>(init, ArrayData::kindToString(kind));
  }
  if (m_sort & HasType) {
    auto const type = reinterpret_cast<const RepoAuthType::Array*>(m_ptr);
    auto const sign = result.empty() ? init : ":";
    result += folly::to<std::string>(sign, show(*type));
  }
  if ((m_sort & IsVanilla) && result.empty()) {
    result += "=Vanilla";
  } else if (!(m_sort & IsVanilla) && !result.empty()) {
    result += "|Bespoke}";
  }
  return result;
}

bool ArraySpec::checkInvariants() const {
  if ((*this == Top()) || (*this == Bottom())) return true;
  assertx(m_sort != IsTop);
  assertx(!(m_sort & IsBottom));
  if (m_sort & HasKind) {
    assertx(isArrayKind(HeaderKind(m_kind)));
    assertx(m_kind != ArrayData::kVecKind &&
            m_kind != ArrayData::kDictKind &&
            m_kind != ArrayData::kKeysetKind);
  } else {
    assertx(m_kind == ArrayData::ArrayKind{});
  }
  if (m_sort & HasType) {
    assertx(m_ptr != 0);
  } else {
    assertx(m_ptr == 0);
  }
  return true;
}

///////////////////////////////////////////////////////////////////////////////
// ClassSpec.

bool ClassSpec::operator<=(const ClassSpec& rhs) const {
  auto const& lhs = *this;

  if (lhs == rhs) return true;
  if (lhs == Bottom() || rhs == Top()) return true;
  if (lhs == Top() || rhs == Bottom()) return false;

  return !rhs.exact() && lhs.cls()->classof(rhs.cls());
}

ClassSpec ClassSpec::operator|(const ClassSpec& rhs) const {
  auto const& lhs = *this;

  if (lhs <= rhs) return rhs;
  if (rhs <= lhs) return lhs;

  assertx(lhs.cls() && rhs.cls());

  // We're unwilling to unify with interfaces, so just return Top.
  if (!isNormalClass(lhs.cls()) || !isNormalClass(rhs.cls())) {
    return Top();
  }

  // Unify to a common ancestor if possible.
  if (auto cls = lhs.cls()->commonAncestor(rhs.cls())) {
    return ClassSpec(cls, ClassSpec::SubTag{});
  }

  return Top();
}

ClassSpec ClassSpec::operator&(const ClassSpec& rhs) const {
  auto const& lhs = *this;

  if (lhs <= rhs) return lhs;
  if (rhs <= lhs) return rhs;

  assertx(lhs.cls() && rhs.cls());

  // If neither class is an interface, their intersection is trivial.
  if (isNormalClass(lhs.cls()) && isNormalClass(rhs.cls())) {
    return Bottom();
  }

  // If either is an interface, we'd need to explore all implementing classes
  // in the program to know if they have a non-empty intersection.  Instead,
  // we'll just try to take the "better" of the two.  We consider a normal
  // class better than an interface, because it might influence important
  // things like method dispatch or property accesses better than an interface
  // type could.
  if (isNormalClass(lhs.cls())) return lhs;
  if (isNormalClass(rhs.cls())) return rhs;

  // If they are both interfaces, we have to pick one arbitrarily, but we must
  // do so in a way that is stable regardless of which one was passed as lhs or
  // rhs (to guarantee that operator& is commutative).  We use the class name
  // in this case to ensure that the ordering is dependent only on the source
  // program (Class* or something like that seems less desirable).
  return lhs.cls()->name()->compare(rhs.cls()->name()) < 0 ? lhs : rhs;
}

std::string ClassSpec::toString() const {
  auto const type = exact() ? "=" : "<=";
  auto const name = cls()->name()->data();
  return folly::to<std::string>(type, name);
}

///////////////////////////////////////////////////////////////////////////////
}}
