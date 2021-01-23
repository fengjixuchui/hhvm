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

#include "hphp/runtime/vm/jit/array-layout.h"

#include "hphp/runtime/base/bespoke/layout.h"
#include "hphp/runtime/base/bespoke/logging-array.h"
#include "hphp/runtime/base/bespoke/logging-profile.h"
#include "hphp/runtime/base/bespoke/monotype-dict.h"
#include "hphp/runtime/base/bespoke/monotype-vec.h"
#include "hphp/runtime/base/bespoke-array.h"
#include "hphp/runtime/vm/jit/irgen-internal.h"
#include "hphp/runtime/vm/jit/prof-data-serialize.h"
#include "hphp/runtime/vm/jit/ssa-tmp.h"

namespace HPHP { namespace jit {

//////////////////////////////////////////////////////////////////////////////

namespace {

using Sort = ArrayLayout::Sort;

auto constexpr kBasicSortMask    = 0b11;
auto constexpr kBasicSortShift   = 0b11;
auto constexpr kBasicSortUnshift = 0b01;

// A "basic sort" is just one of the four named Sort enum values. If `sort`
// is non-basic, then Sort::Bottom < sort < Sort::Bespoke.
constexpr bool isBasicSort(Sort sort) {
  return sort <= Sort::Bespoke;
}

// Converts non-basic sorts (which are subtypes of Bespoke) to Bespoke.
constexpr Sort toBasicSort(Sort sort) {
  return std::min(sort, Sort::Bespoke);
}

// If we mask a basic sort, we'll get a value such that | and & bit ops on
// that value correspond to | and & type operations on the original sort.
constexpr int maskBasicSort(Sort sort) {
  assertx(isBasicSort(sort));
  return kBasicSortMask & (int(sort) + kBasicSortShift);
}

static_assert(maskBasicSort(Sort::Top)     == 0b11);
static_assert(maskBasicSort(Sort::Vanilla) == 0b01);
static_assert(maskBasicSort(Sort::Bespoke) == 0b10);
static_assert(maskBasicSort(Sort::Bottom)  == 0b00);

// This operation is the inverse of the maskBasicSort operation above.
constexpr Sort unmaskBasicSort(int masked) {
  auto const result = Sort(kBasicSortMask & (masked + kBasicSortUnshift));
  assertx(isBasicSort(result));
  return result;
}

static_assert(unmaskBasicSort(maskBasicSort(Sort::Top))     == Sort::Top);
static_assert(unmaskBasicSort(maskBasicSort(Sort::Vanilla)) == Sort::Vanilla);
static_assert(unmaskBasicSort(maskBasicSort(Sort::Bespoke)) == Sort::Bespoke);
static_assert(unmaskBasicSort(maskBasicSort(Sort::Bottom))  == Sort::Bottom);

// Returns the basic sort that is the intersection of the given basic sorts.
constexpr Sort intersectBasicSort(Sort a, Sort b) {
  return unmaskBasicSort(maskBasicSort(a) & maskBasicSort(b));
}

// Returns the basic sort that is the union of the given basic sorts.
constexpr Sort unionBasicSort(Sort a, Sort b) {
  return unmaskBasicSort(maskBasicSort(a) | maskBasicSort(b));
}

// Returns the sort (either Bespoke, or non-basic) for this bespoke layout.
Sort sortFromLayoutIndex(bespoke::LayoutIndex index) {
  return Sort(index.raw + int(Sort::Bespoke));
}

const bespoke::Layout& assertBespoke(ArrayLayout layout) {
  auto const result = layout.bespokeLayout();
  assertx(result != nullptr);
  return *result;
}

}

//////////////////////////////////////////////////////////////////////////////

ArrayLayout::ArrayLayout(bespoke::LayoutIndex index)
  : sort(sortFromLayoutIndex(index))
{
  assertx(bespoke::Layout::FromIndex(*layoutIndex()));
}

ArrayLayout::ArrayLayout(const bespoke::Layout* layout)
  : sort(sortFromLayoutIndex(layout->index()))
{
  assertx(bespoke::Layout::FromIndex(*layoutIndex()));
}

bool ArrayLayout::operator<=(const ArrayLayout& o) const {
  if (*this == o) return true;
  if (o == Top()) return true;
  if (*this == Bottom()) return true;

  // The max chain length on basic sorts alone is three:
  //
  //   Bottom < {Vanilla,Bespoke} < Top
  //
  // We took care of the Bottom, Top, and equality cases above. Further, if o
  // is non-basic, it's a strict subtype of Bespoke. So we can return here.
  if (isBasicSort(sort)) return false;

  if (isBasicSort(o.sort)) return o == Bespoke();
  return assertBespoke(*this) <= assertBespoke(o);
}

ArrayLayout ArrayLayout::operator|(const ArrayLayout& o) const {
  if (*this == o) return o;
  if (o == Bottom()) return *this;
  if (*this == Bottom()) return o;

  // If either side is captured as a basic sort, then the result is, too.
  if (isBasicSort(sort) || isBasicSort(o.sort)) {
    return ArrayLayout(unionBasicSort(toBasicSort(sort), toBasicSort(o.sort)));
  }

  return ArrayLayout(assertBespoke(*this) | assertBespoke(o));
}

ArrayLayout ArrayLayout::operator&(const ArrayLayout& o) const {
  if (*this == o) return o;
  if (o == Top()) return *this;
  if (*this == Top()) return o;

  // We only intersect bespoke layouts if toBasicSort is Bespoke for both.
  auto const meet = intersectBasicSort(toBasicSort(sort), toBasicSort(o.sort));
  if (meet != Sort::Bespoke) return ArrayLayout(meet);

  // If either type is Bespoke (i.e. "bespoke top"), return the other type.
  if (o == Bespoke()) return *this;
  if (*this == Bespoke()) return o;
  auto const result = assertBespoke(*this) & assertBespoke(o);
  return result ? ArrayLayout(result) : Bottom();
}

bool ArrayLayout::logging() const {
  auto const index = layoutIndex();
  return index && *index == bespoke::LoggingArray::GetLayoutIndex();
}

bool ArrayLayout::monotype() const {
  auto const index = layoutIndex();
  if (!index) return false;
  return bespoke::isMonotypeVecLayout(*index) ||
         bespoke::isMonotypeDictLayout(*index);
}

const bespoke::Layout* ArrayLayout::bespokeLayout() const {
  auto const index = layoutIndex();
  if (!index) return nullptr;
  return bespoke::Layout::FromIndex(*index);
}

folly::Optional<bespoke::LayoutIndex> ArrayLayout::layoutIndex() const {
  auto const index = int(sort) - int(Sort::Bespoke);
  if (index < 0) return {};
  return bespoke::LayoutIndex { safe_cast<uint16_t>(index) };
}

MaskAndCompare ArrayLayout::bespokeMaskAndCompare() const {
  auto const& layout = assertBespoke(*this);
  if (isBasicSort(sort)) return MaskAndCompare{0,0,0};
  return layout.maskAndCompare();
}

const bespoke::Layout* ArrayLayout::irgenLayout() const {
  auto const index = std::max(int(sort) - int(Sort::Bespoke), 0);
  return bespoke::Layout::FromIndex({safe_cast<uint16_t>(index)});
}

std::string ArrayLayout::describe() const {
  if (isBasicSort(sort)) {
    switch (sort) {
      case Sort::Top:     return "Top";
      case Sort::Vanilla: return "Vanilla";
      case Sort::Bespoke: return "Bespoke";
      case Sort::Bottom:  return "Bottom";
    }
  }
  return folly::sformat("Bespoke({})", assertBespoke(*this).describe());
}

ArrayData* ArrayLayout::apply(ArrayData* ad) const {
  assertx(ad->isStatic());
  assertx(ad->isVanilla());

  auto const result = [&]() -> ArrayData* {
    if (vanilla() || logging()) return ad;
    if (monotype()) return bespoke::maybeMonoify(ad);
    return nullptr;
  }();

  SCOPE_ASSERT_DETAIL("ArrayLayout::apply") { return describe(); };
  always_assert(result != nullptr);
  return result;
}

//////////////////////////////////////////////////////////////////////////////

ArrayLayout ArrayLayout::appendType(Type val) const {
  if (vanilla()) return ArrayLayout::Vanilla();
  if (isBasicSort(sort)) return ArrayLayout::Top();
  return bespokeLayout()->appendType(val);
}

ArrayLayout ArrayLayout::removeType(Type key) const {
  if (vanilla()) return ArrayLayout::Vanilla();
  if (isBasicSort(sort)) return ArrayLayout::Top();
  return bespokeLayout()->removeType(key);
}

ArrayLayout ArrayLayout::setType(Type key, Type val) const {
  if (vanilla()) return ArrayLayout::Vanilla();
  if (isBasicSort(sort)) return ArrayLayout::Top();
  return bespokeLayout()->setType(key, val);
}

std::pair<Type, bool> ArrayLayout::elemType(Type key) const {
  if (isBasicSort(sort)) return {TInitCell, false};
  return bespokeLayout()->elemType(key);
}

std::pair<Type, bool> ArrayLayout::firstLastType(
    bool isFirst, bool isKey) const {
  if (isBasicSort(sort)) return {isKey ? (TInt | TStr) : TInitCell, false};
  return bespokeLayout()->firstLastType(isFirst, isKey);
}

Type ArrayLayout::iterPosType(Type pos, bool isKey) const {
  if (isBasicSort(sort)) return isKey ? (TInt | TStr) : TInitCell;
  return bespokeLayout()->iterPosType(pos, isKey);
}

//////////////////////////////////////////////////////////////////////////////

namespace {
using bespoke::LoggingProfileKey;
using bespoke::SinkProfileKey;

void write_source_key(ProfDataSerializer& ser, const LoggingProfileKey& key) {
  write_raw(ser, key.slot);
  if (key.slot == kInvalidSlot) {
    write_srckey(ser, key.sk);
  } else {
    write_class(ser, key.cls);
  }
}

LoggingProfileKey read_source_key(ProfDataDeserializer& des) {
  LoggingProfileKey key(SrcKey{});
  read_raw(des, key.slot);
  if (key.slot == kInvalidSlot) {
    key.sk = read_srckey(des);
  } else {
    key.cls = read_class(des);
  }
  return key;
}

void write_sink_key(ProfDataSerializer& ser, const SinkProfileKey& key) {
  write_raw(ser, key.first);
  write_srckey(ser, key.second);
}

SinkProfileKey read_sink_key(ProfDataDeserializer& des) {
  auto const trans = read_raw<TransID>(des);
  return SinkProfileKey(trans, read_srckey(des));
}
}

void serializeBespokeLayouts(ProfDataSerializer& ser) {
  write_raw(ser, bespoke::countSources());
  bespoke::eachSource([&](auto const& profile) {
    write_source_key(ser, profile.key);
    write_raw(ser, profile.layout);
  });
  write_raw(ser, bespoke::countSinks());
  bespoke::eachSink([&](auto const& profile) {
    write_sink_key(ser, profile.key);
    write_raw(ser, profile.layout);
  });
}

void deserializeBespokeLayouts(ProfDataDeserializer& des) {
  auto const sources = read_raw<size_t>(des);
  for (auto i = 0; i < sources; i++) {
    auto const key = read_source_key(des);
    bespoke::deserializeSource(key, read_layout(des));
  }
  auto const sinks = read_raw<size_t>(des);
  for (auto i = 0; i < sinks; i++) {
    auto const key = read_sink_key(des);
    bespoke::deserializeSink(key, read_layout(des));
  }
  bespoke::Layout::FinalizeHierarchy();
}

//////////////////////////////////////////////////////////////////////////////

}}
