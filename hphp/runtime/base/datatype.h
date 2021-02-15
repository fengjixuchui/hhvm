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

#pragma once

#include <cstdint>
#include <cstdio>
#include <string>

#include <folly/Format.h>
#include <folly/Optional.h>

#include "hphp/util/assertions.h"
#include "hphp/util/low-ptr.h"
#include "hphp/util/portability.h"

namespace HPHP {

///////////////////////////////////////////////////////////////////////////////

constexpr size_t kDataTypePopCount = 3;

// udt meaning "unordered DataType": compute an encoding of DataTypes into a
// 3-of-7 balanced (and thus, unordered) error-correcting code.
//
// This function returns the `index`th codeword, setting the lowest bit based
// on the bool `counted`. To construct a persistent/counted DataType pair,
// call it twice with the same index but different values for counted.
constexpr int8_t udt(size_t index, bool counted) {
  for (auto i = 0; i <= std::numeric_limits<uint8_t>::max(); i += 2) {
    if (folly::popcount(i) != kDataTypePopCount) continue;
    if (index == 0) return static_cast<int8_t>(i | (counted ? 1 : 0));
    index--;
  }
  // We've run out of codewords. clang allows us to use an always_assert here.
  // GCC does not - if we use an assert, the function is no longer constexpr.
#ifdef __clang__
  always_assert(false);
#else
  return 0;
#endif
}

/*
 * DataType is the type tag for a TypedValue (see typed-value.h).
 *
 * If you want to add a new type, beware of the following restrictions:
 * - KindOfUninit must be 0. Many places rely on zero-initialized memory
 *   being a valid, KindOfUninit TypedValue.
 * - KindOfNull must be 2, and 1 must not be a valid type. This allows for
 *   a fast implementation of isNullType().
 * - The Array and String types are positioned to allow for fast array/string
 *   checks, ignoring persistence (see isArrayType and isStringType).
 * - Refcounted types are odd, and uncounted types are even, to allow fast
 *   countness checks.
 * - Types with persistent and non-persistent versions must be negative, for
 *   equivDataTypes(). Other types may be negative, as long as dropping the low
 *   bit does not give another valid type.
 * - -128 and -127 are used as invalid types and can't be real DataTypes.
 *
 * If you think you need to change any of these restrictions, be prepared to
 * deal with subtle bugs and/or performance regressions while you sort out the
 * consequences. At a minimum, you must:
 * - Audit every helper function in this file.
 * - Audit jit::emitTypeTest().
 */
#define DATATYPES \
  DT(PersistentDArray, udt(0,  false)) \
  DT(DArray,           udt(0,  true))  \
  DT(PersistentVArray, udt(1,  false)) \
  DT(VArray,           udt(1,  true))  \
  DT(PersistentDict,   udt(2,  false)) \
  DT(Dict,             udt(2,  true))  \
  DT(PersistentVec,    udt(3,  false)) \
  DT(Vec,              udt(3,  true))  \
  DT(PersistentKeyset, udt(4,  false)) \
  DT(Keyset,           udt(4,  true))  \
  DT(Record,           udt(5,  true))  \
  DT(PersistentString, udt(6,  false)) \
  DT(String,           udt(6,  true))  \
  DT(Object,           udt(7,  true))  \
  DT(Resource,         udt(8,  true))  \
  DT(RFunc,            udt(9,  true))  \
  DT(RClsMeth,         udt(10, true))  \
  DT(ClsMeth,          udt(11, !use_lowptr)) \
  DT(Boolean,          udt(12, false)) \
  DT(Int64,            udt(13, false)) \
  DT(Double,           udt(14, false)) \
  DT(Func,             udt(15, false)) \
  DT(Class,            udt(16, false)) \
  DT(LazyClass,        udt(17, false)) \
  DT(Uninit,           udt(18, false)) \
  DT(Null,             udt(19, false))

enum class DataType : int8_t {
#define DT(name, value) name = value,
DATATYPES
#undef DT
};

using data_type_t = typename std::underlying_type<DataType>::type;

// Macro so we can limit its scope to this file. Anyone else doing this cast
// should have to write out the whole thing and think about their life choices.
#define dt_t(t) static_cast<data_type_t>(t)
#define ut_t(t) static_cast<std::make_unsigned<data_type_t>::type>(t)

/*
 * Also define KindOf<Foo> for each type, to avoid having to change thousands
 * of existing usage sites.
 */
#define DT(name, ...) auto constexpr KindOf##name = DataType::name;
DATATYPES
#undef DT

/*
 * Sentinel invalid DataTypes.
 *
 * These values must differ from that of any real DataType.  A live TypedValue
 * should never have these as its type tag, so we keep them out of the enum to
 * keep switches cleaner.
 *
 * These should only be used where MaybeDataType cannot be (e.g., in
 * TypedValues, such as for MixedArray tombstones).
 */
constexpr DataType kInvalidDataType = static_cast<DataType>(-128);
constexpr DataType kExtraInvalidDataType = static_cast<DataType>(0);

/*
 * DataType limits.
 */
auto constexpr kMinDataType = ut_t(KindOfPersistentDArray);
auto constexpr kMaxDataType = ut_t(KindOfNull);
auto constexpr kMinRefCountedDataType = ut_t(KindOfDArray);
auto constexpr kMaxRefCountedDataType =
  use_lowptr ? ut_t(KindOfRClsMeth) : ut_t(KindOfClsMeth);

/*
 * A DataType is a refcounted type if and only if it has this bit set.
 */
constexpr int kRefCountedBit = 0x1;

/*
 * Whether a type is refcounted.
 */
constexpr bool isRefcountedType(DataType t) {
  return ut_t(t) & kRefCountedBit;
}

/*
 * Whether a type is or has a persistent version.
 */
constexpr bool hasPersistentFlavor(DataType t) {
  return ut_t(t) <= ut_t(KindOfString);
}

/*
 * Return `dt` with or without the refcount bit set.
 */
constexpr DataType dt_with_rc(DataType dt) {
  assertx(hasPersistentFlavor(dt) || isRefcountedType(dt));
  return static_cast<DataType>(dt_t(dt) | kRefCountedBit);
}
constexpr DataType dt_with_persistence(DataType dt) {
  assertx(hasPersistentFlavor(dt) || !isRefcountedType(dt));
  return static_cast<DataType>(dt_t(dt) & ~kRefCountedBit);
}

/*
 * Return the ref-counted flavor of `dt` if it has both a KindOf$x and a
 * KindOfPersistent$x flavor
 */
constexpr DataType dt_modulo_persistence(DataType dt) {
  return hasPersistentFlavor(dt) ? dt_with_rc(dt) : dt;
}

///////////////////////////////////////////////////////////////////////////////
/*
 * Optional DataType.
 *
 * Used for (DataType|KindOfNoneType) or (DataType|KindOfAnyType), depending on
 * context.  Users who wish to use (DataType|KindOfNoneType|KindOfAnyType)
 * should consider dying in a fire.
 */
using MaybeDataType = folly::Optional<DataType>;

/*
 * Extracts the DataType from the given type
 */
MaybeDataType get_datatype(
  const std::string& name,
  bool can_be_collection,
  bool is_nullable,
  bool is_soft
);

///////////////////////////////////////////////////////////////////////////////
// DataTypeCategory

// These categories must be kept in order from least to most specific.
#define DT_CATEGORIES(func)                     \
  func(Generic)                                 \
  func(IterBase)                                \
  func(CountnessInit)                           \
  func(Specific)                                \
  func(Specialized)

enum class DataTypeCategory : uint8_t {
#define DT(name) name,
  DT_CATEGORIES(DT)
#undef DT
};

#define DT(name) auto constexpr DataType##name = DataTypeCategory::name;
DT_CATEGORIES(DT)
#undef DT

///////////////////////////////////////////////////////////////////////////////
// Names.

inline std::string tname(DataType t) {
  switch (t) {
#define DT(name, ...) case KindOf##name: return #name;
DATATYPES
#undef DT
    default: {
      if (t == kInvalidDataType) return "Invalid";
      return folly::sformat("Unknown:{}", static_cast<int>(t));
    }
  }
}

inline std::string typeCategoryName(DataTypeCategory c) {
  switch (c) {
# define CASE(name) case DataType##name: return "DataType" #name;
  DT_CATEGORIES(CASE)
#undef CASE
  }
  not_reached();
}

/*
 * These are used in type-variant.cpp.
 */
constexpr int kDestrTableSize =
  (kMaxRefCountedDataType - kMinRefCountedDataType) / 2 + 1;

constexpr unsigned typeToDestrIdx(DataType t) {
  // t must be a refcounted type, but we can't actually assert that and still
  // be constexpr.
  return (static_cast<int64_t>(t) - kMinRefCountedDataType) / 2;
}

///////////////////////////////////////////////////////////////////////////////
// Is-a macros.

/*
 * Whether a type is valid.
 */
constexpr bool isRealType(DataType t) {
  return ut_t(t) >= kMinDataType && ut_t(t) <= kMaxDataType &&
         folly::popcount(ut_t(t) & ~kRefCountedBit) == kDataTypePopCount;
}

/*
 * Whether a builtin return or param type is not a simple type.
 *
 * This is different from isRefcountedType because builtins can accept and
 * return Variants, and we use folly::none to denote these cases.
 */
inline bool isBuiltinByRef(MaybeDataType t) {
  return t != KindOfNull &&
         t != KindOfBoolean &&
         t != KindOfInt64 &&
         t != KindOfDouble;
}

/*
 * Whether a type's value is an integral value in m_data.num.
 */
constexpr bool hasNumData(DataType t) {
  return t == KindOfBoolean || t == KindOfInt64;
}

/*
 * Whether a type is KindOfUninit or KindOfNull.
 */
constexpr bool isNullType(DataType t) {
  return ut_t(t) >= ut_t(KindOfUninit);
}

/*
 * Whether a type is any kind of string or array.
 */
constexpr bool isStringType(DataType t) {
  return !(ut_t(t) & ~ut_t(KindOfString));
}
inline bool isStringType(MaybeDataType t) {
  return t && isStringType(*t);
}

constexpr bool isArrayLikeType(DataType t) {
  return ut_t(t) <= ut_t(KindOfKeyset);
}
inline bool isArrayLikeType(MaybeDataType t) {
  return t && isArrayLikeType(*t);
}

/*
 * When any dvarray will do.
 */
constexpr bool isPHPArrayType(DataType t) {
  return ut_t(t) <= ut_t(KindOfVArray);
}
inline bool isPHPArrayType(MaybeDataType t) {
  return t && isPHPArrayType(*t);
}

constexpr bool isVecOrVArrayType(DataType t) {
  auto const dt = static_cast<DataType>(dt_t(t) & ~kRefCountedBit);
  return dt == KindOfPersistentVArray || dt == KindOfPersistentVec;
}
inline bool isVecOrVArrayType(MaybeDataType t) {
  return t && isVecOrVArrayType(*t);
}

constexpr bool isDictOrDArrayType(DataType t) {
  auto const dt = static_cast<DataType>(dt_t(t) & ~kRefCountedBit);
  return dt == KindOfPersistentDArray || dt == KindOfPersistentDict;
}
inline bool isDictOrDArrayType(MaybeDataType t) {
  return t && isDictOrDArrayType(*t);
}

/*
 * Currently matches any PHP dvarray. This method will go away.
 */
constexpr bool isArrayType(DataType t) {
  return isPHPArrayType(t);
}
inline bool isArrayType(MaybeDataType t) {
  return t && isArrayType(*t);
}

constexpr bool isHackArrayType(DataType t) {
  return ut_t(t) >= ut_t(KindOfPersistentDict) && ut_t(t) <= ut_t(KindOfKeyset);
}
inline bool isHackArrayType(MaybeDataType t) {
  return t && isHackArrayType(*t);
}

constexpr bool isVecType(DataType t) {
  return !(ut_t(t) & ~ut_t(KindOfVec));
}
inline bool isVecType(MaybeDataType t) {
  return t && isVecType(*t);
}

constexpr bool isDictType(DataType t) {
  return !(ut_t(t) & ~ut_t(KindOfDict));
}
inline bool isDictType(MaybeDataType t) {
  return t && isDictType(*t);
}

constexpr bool isKeysetType(DataType t) {
  return !(ut_t(t) & ~ut_t(KindOfKeyset));
}
inline bool isKeysetType(MaybeDataType t) {
  return t && isKeysetType(*t);
}

/*
 * Other type-check functions.
 */
constexpr bool isIntType(DataType t) { return t == KindOfInt64; }
constexpr bool isBoolType(DataType t) { return t == KindOfBoolean; }
constexpr bool isDoubleType(DataType t) { return t == KindOfDouble; }
constexpr bool isObjectType(DataType t) { return t == KindOfObject; }
constexpr bool isRecordType(DataType t) { return t == KindOfRecord; }
constexpr bool isResourceType(DataType t) { return t == KindOfResource; }
constexpr bool isRFuncType(DataType t) { return t == KindOfRFunc; }
constexpr bool isFuncType(DataType t) { return t == KindOfFunc; }
constexpr bool isClassType(DataType t) { return t == KindOfClass; }
constexpr bool isClsMethType(DataType t) { return t == KindOfClsMeth; }
constexpr bool isRClsMethType(DataType t) { return t == KindOfRClsMeth; }
constexpr bool isLazyClassType(DataType t) { return t == KindOfLazyClass; }

/*
 * Return whether two DataTypes for primitive types are "equivalent" as far as
 * user-visible PHP types are concerned (i.e. the same modulo countedness).
 * Note that KindOfUninit and KindOfNull are not considered equivalent.
 */
constexpr bool equivDataTypes(DataType t1, DataType t2) {
  return !((ut_t(t1) ^ ut_t(t2)) & ~kRefCountedBit);
}

/*
 * If you think you need to do any of these operations, you should instead add
 * a helper function up above and call that, to keep any knowledge about the
 * relative values of DataTypes in this file.
 */
bool operator<(DataType, DataType) = delete;
bool operator>(DataType, DataType) = delete;
bool operator<=(DataType, DataType) = delete;
bool operator>=(DataType, DataType) = delete;

#undef ut_t
#undef dt_t

///////////////////////////////////////////////////////////////////////////////
// Switch case macros.

/*
 * Covers all DataTypes `dt' such that !isRefcountedType(dt) holds.
 */
#define DT_UNCOUNTED_CASE   \
  case KindOfUninit:        \
  case KindOfNull:          \
  case KindOfBoolean:       \
  case KindOfInt64:         \
  case KindOfDouble:        \
  case KindOfPersistentString:  \
  case KindOfPersistentVArray:  \
  case KindOfPersistentDArray:  \
  case KindOfPersistentVec: \
  case KindOfPersistentDict: \
  case KindOfPersistentKeyset: \
  case KindOfFunc:          \
  case KindOfClass:         \
  case KindOfLazyClass
}

///////////////////////////////////////////////////////////////////////////////

namespace folly {
template<> class FormatValue<HPHP::DataTypeCategory> {
 public:
  explicit FormatValue(HPHP::DataTypeCategory val) noexcept : m_val(val) {}

  template<typename Callback>
  void format(FormatArg& arg, Callback& cb) const {
    format_value::formatString(typeCategoryName(m_val), arg, cb);
  }

 private:
  HPHP::DataTypeCategory m_val;
};

template<> class FormatValue<HPHP::DataType> {
 public:
  explicit FormatValue(HPHP::DataType dt) noexcept : m_dt(dt) {}

  template<typename C>
  void format(FormatArg& arg, C& cb) const {
    format_value::formatString(tname(m_dt), arg, cb);
  }

 private:
  HPHP::DataType m_dt;
};
}

///////////////////////////////////////////////////////////////////////////////

