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

#include "hphp/runtime/base/apc-stats.h"
#include "hphp/runtime/base/array-data-defs.h"
#include "hphp/runtime/base/bespoke-array.h"
#include "hphp/runtime/base/bespoke/layout.h"
#include "hphp/runtime/base/bespoke/logging-array.h"
#include "hphp/runtime/base/mixed-array-defs.h"
#include "hphp/runtime/base/sort-flags.h"
#include "hphp/runtime/base/tv-refcount.h"

namespace HPHP {

//////////////////////////////////////////////////////////////////////////////

namespace {
using bespoke::g_layout_funcs;

uint8_t getLayoutByte(const ArrayData* ad) {
  auto const result = BespokeArray::asBespoke(ad)->layoutIndex().byte();
  assertx(g_layout_funcs.fnRelease[result] != nullptr);
  return result;
}
}

BespokeArray* BespokeArray::asBespoke(ArrayData* ad) {
  auto ret = reinterpret_cast<BespokeArray*>(ad);
  assertx(ret->checkInvariants());
  return ret;
}
const BespokeArray* BespokeArray::asBespoke(const ArrayData* ad) {
  return asBespoke(const_cast<ArrayData*>(ad));
}

bespoke::LayoutIndex BespokeArray::layoutIndex() const {
  return {m_extra_hi16};
}

const bespoke::LayoutFunctions* BespokeArray::vtable() const {
  return bespoke::ConcreteLayout::FromConcreteIndex(layoutIndex())->vtable();
}

void BespokeArray::setLayoutIndex(bespoke::LayoutIndex index) {
  m_extra_hi16 = index.raw;
}

size_t BespokeArray::heapSize() const {
  return g_layout_funcs.fnHeapSize[getLayoutByte(this)](this);
}
void BespokeArray::scan(type_scan::Scanner& scan) const {
  return g_layout_funcs.fnScan[getLayoutByte(this)](this, scan);
}

ArrayData* BespokeArray::ToVanilla(const ArrayData* ad, const char* reason) {
  return g_layout_funcs.fnEscalateToVanilla[getLayoutByte(ad)](ad, reason);
}

bool BespokeArray::checkInvariants() const {
  assertx(!isVanilla());
  assertx(kindIsValid());
  assertx(!isSampledArray());
  static_assert(ArrayData::kDefaultVanillaArrayExtra == uint32_t(-1));
  DEBUG_ONLY auto constexpr kVanillaLayoutIndex = uint16_t(-1);
  assertx(m_extra_hi16 != kVanillaLayoutIndex);
  return true;
}

//////////////////////////////////////////////////////////////////////////////

ArrayData* BespokeArray::MakeUncounted(ArrayData* ad, bool hasApcTv,
                                       DataWalker::PointerMap* seen) {
  assertx(ad->isRefCounted());

  auto const vad = ToVanilla(ad, "BespokeArray::MakeUncounted");
  SCOPE_EXIT { decRefArr(vad); };

  if (seen) {
    auto const mark = [&](TypedValue tv) {
      if (isRefcountedType(type(tv)) && val(tv).pcnt->hasMultipleRefs()) {
        seen->insert({val(tv).pcnt, nullptr});
      }
    };
    if (vad->hasMultipleRefs()) seen->insert({vad, nullptr});
    IterateKVNoInc(vad, [&](auto k, auto v) { mark(k); mark(v); });
  }

  if (vad->hasVanillaPackedLayout()) {
    return PackedArray::MakeUncounted(vad, hasApcTv, seen);
  } else if (vad->hasVanillaMixedLayout()) {
    return MixedArray::MakeUncounted(vad, hasApcTv, seen);
  }
  return SetArray::MakeUncounted(vad, hasApcTv, seen);
}

void BespokeArray::ReleaseUncounted(ArrayData* ad) {
  if (!ad->uncountedDecRef()) return;
  auto const byte = getLayoutByte(ad);
  g_layout_funcs.fnReleaseUncounted[byte](ad);
  if (APCStats::IsCreated()) {
    APCStats::getAPCStats().removeAPCUncountedBlock();
  }
  auto const bytes = g_layout_funcs.fnHeapSize[byte](ad);
  auto const extra = uncountedAllocExtra(ad, ad->hasApcTv());
  uncounted_sized_free(reinterpret_cast<char*>(ad) - extra, bytes + extra);
}

//////////////////////////////////////////////////////////////////////////////

// ArrayData interface
void BespokeArray::Release(ArrayData* ad) {
  g_layout_funcs.fnRelease[getLayoutByte(ad)](ad);
}
bool BespokeArray::IsVectorData(const ArrayData* ad) {
  return g_layout_funcs.fnIsVectorData[getLayoutByte(ad)](ad);
}

// RO access
TypedValue BespokeArray::NvGetInt(const ArrayData* ad, int64_t key) {
  return g_layout_funcs.fnNvGetInt[getLayoutByte(ad)](ad, key);
}
TypedValue BespokeArray::NvGetStr(const ArrayData* ad, const StringData* key) {
  return g_layout_funcs.fnNvGetStr[getLayoutByte(ad)](ad, key);
}
TypedValue BespokeArray::GetPosKey(const ArrayData* ad, ssize_t pos) {
  return g_layout_funcs.fnGetPosKey[getLayoutByte(ad)](ad, pos);
}
TypedValue BespokeArray::GetPosVal(const ArrayData* ad, ssize_t pos) {
  return g_layout_funcs.fnGetPosVal[getLayoutByte(ad)](ad, pos);
}
bool BespokeArray::ExistsInt(const ArrayData* ad, int64_t key) {
  return NvGetInt(ad, key).is_init();
}
bool BespokeArray::ExistsStr(const ArrayData* ad, const StringData* key) {
  return NvGetStr(ad, key).is_init();
}

// iteration
ssize_t BespokeArray::IterBegin(const ArrayData* ad) {
  return g_layout_funcs.fnIterBegin[getLayoutByte(ad)](ad);
}
ssize_t BespokeArray::IterLast(const ArrayData* ad) {
  return g_layout_funcs.fnIterLast[getLayoutByte(ad)](ad);
}
ssize_t BespokeArray::IterEnd(const ArrayData* ad) {
  return g_layout_funcs.fnIterEnd[getLayoutByte(ad)](ad);
}
ssize_t BespokeArray::IterAdvance(const ArrayData* ad, ssize_t pos) {
  return g_layout_funcs.fnIterAdvance[getLayoutByte(ad)](ad, pos);
}
ssize_t BespokeArray::IterRewind(const ArrayData* ad, ssize_t pos) {
  return g_layout_funcs.fnIterRewind[getLayoutByte(ad)](ad, pos);
}

// RW access
arr_lval BespokeArray::LvalInt(ArrayData* ad, int64_t key) {
  return g_layout_funcs.fnLvalInt[getLayoutByte(ad)](ad, key);
}
arr_lval BespokeArray::LvalStr(ArrayData* ad, StringData* key) {
  return g_layout_funcs.fnLvalStr[getLayoutByte(ad)](ad, key);
}
tv_lval BespokeArray::ElemInt(
    tv_lval lval, int64_t key, bool throwOnMissing) {
  auto const ad = lval.val().parr;
  return g_layout_funcs.fnElemInt[getLayoutByte(ad)](lval, key, throwOnMissing);
}
tv_lval BespokeArray::ElemStr(
    tv_lval lval, StringData* key, bool throwOnMissing) {
  auto const ad = lval.val().parr;
  return g_layout_funcs.fnElemStr[getLayoutByte(ad)](lval, key, throwOnMissing);
}

// insertion
ArrayData* BespokeArray::SetIntMove(ArrayData* ad, int64_t key, TypedValue v) {
  return g_layout_funcs.fnSetIntMove[getLayoutByte(ad)](ad, key, v);
}
ArrayData* BespokeArray::SetStrMove(ArrayData* ad, StringData* key, TypedValue v) {
  return g_layout_funcs.fnSetStrMove[getLayoutByte(ad)](ad, key, v);
}

// deletion
ArrayData* BespokeArray::RemoveInt(ArrayData* ad, int64_t key) {
  return g_layout_funcs.fnRemoveInt[getLayoutByte(ad)](ad, key);
}
ArrayData* BespokeArray::RemoveStr(ArrayData* ad, const StringData* key) {
  return g_layout_funcs.fnRemoveStr[getLayoutByte(ad)](ad, key);
}

// sorting
ArrayData* BespokeArray::EscalateForSort(ArrayData* ad, SortFunction sf) {
  if (!isSortFamily(sf)) {
    if (ad->isVArray())  return ad->toDArray(true);
    if (ad->isVecType()) return ad->toDict(true);
  }
  assertx(!ad->empty());
  return g_layout_funcs.fnPreSort[getLayoutByte(ad)](ad, sf);
}
ArrayData* BespokeArray::PostSort(ArrayData* ad, ArrayData* vad) {
  assertx(vad->isVanilla());
  if (ad->toDataType() != vad->toDataType()) return vad;
  assertx(vad->hasExactlyOneRef());
  return g_layout_funcs.fnPostSort[getLayoutByte(ad)](ad, vad);
}

// high-level ops
ArrayData* BespokeArray::AppendMove(ArrayData* ad, TypedValue v) {
  return g_layout_funcs.fnAppendMove[getLayoutByte(ad)](ad, v);
}
ArrayData* BespokeArray::Pop(ArrayData* ad, Variant& out) {
  return g_layout_funcs.fnPop[getLayoutByte(ad)](ad, out);
}
void BespokeArray::OnSetEvalScalar(ArrayData*) {
  always_assert(false);
}

// copies and conversions
ArrayData* BespokeArray::CopyStatic(const ArrayData*) {
  always_assert(false);
}
ArrayData* BespokeArray::ToDVArray(ArrayData* ad, bool copy) {
  return g_layout_funcs.fnToDVArray[getLayoutByte(ad)](ad, copy);
}
ArrayData* BespokeArray::ToHackArr(ArrayData* ad, bool copy) {
  return g_layout_funcs.fnToHackArr[getLayoutByte(ad)](ad, copy);
}
ArrayData* BespokeArray::SetLegacyArray(ArrayData* ad, bool copy, bool legacy) {
  return g_layout_funcs.fnSetLegacyArray[getLayoutByte(ad)](ad, copy, legacy);
}

//////////////////////////////////////////////////////////////////////////////

}
