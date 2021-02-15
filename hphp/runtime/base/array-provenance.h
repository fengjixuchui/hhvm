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

#include "hphp/runtime/base/runtime-option.h"
#include "hphp/runtime/base/static-string-table.h"
#include "hphp/runtime/base/typed-value.h"
#include "hphp/runtime/base/types.h"

#include "hphp/util/low-ptr.h"
#include "hphp/util/rds-local.h"

#include <folly/Format.h>
#include <folly/Optional.h>

namespace HPHP {

struct APCArray;
struct ArrayData;
struct StringData;
struct c_WaitableWaitHandle;
struct AsioExternalThreadEvent;
struct SrcKey;

namespace arrprov {

///////////////////////////////////////////////////////////////////////////////

/*
 * A provenance annotation
 *
 * We store filenames and line numbers rather than units since we need to
 * manipulate these tags during the repo build. Additionally, we also have
 * several tag types denoting explicitly unknown tags: e.g. when a tag is a
 * result of a union of otherwise-identical arrays in the repo build.
 */
struct Tag {
  enum class Kind {
    /* uninitialized */
    Invalid,
    /* lost original line number as a result of trait ${x}init merges */
    KnownTraitMerge,
    /* Dummy tag for all large enums, which we cache as static arrays */
    KnownLargeEnum,
    /* a particular argument to a function should be marked */
    KnownFuncParam,
    /* no vmregs are available, filename and line are runtime locations */
    RuntimeLocation,
    /* some piece of the runtime prevented a backtrace from being collected--
     * e.g. the JIT will use this to prevent a tag being assigned to an array
     * in the JIT corresponding to the PHP location that entered the JIT */
    RuntimeLocationPoison,
    /* known unit + line number */
    Known,
    /* NOTE: We CANNOT fit another kind here; kind 7 is reserved */
  };

  constexpr Tag() = default;
  Tag(const Func* func, Offset offset);

  static Tag Param(const Func* func, int32_t param);

  static Tag Param(const StringData* func, int32_t param) {
    return Tag(Kind::KnownFuncParam, func, param);
  }
  static Tag Known(const StringData* filename, int32_t line) {
    return Tag(Kind::Known, filename, line);
  }
  static Tag TraitMerge(const StringData* filename) {
    return Tag(Kind::KnownTraitMerge, filename);
  }
  static Tag LargeEnum(const StringData* classname) {
    return Tag(Kind::KnownLargeEnum, classname);
  }
  static Tag RuntimeLocation(const StringData* filename) {
    return Tag(Kind::RuntimeLocation, filename);
  }
  static Tag RuntimeLocationPoison(const StringData* filename) {
    return Tag(Kind::RuntimeLocationPoison, filename);
  }

  /*
   * `name` means different things for different kinds:
   *  - Kind::Known, Kind::KnownTraitMerge: a Hack filename
   *  - Kind::KnownLargeEnum: a Hack enum class
   *  - Kind::KnownFuncParam: a Hack function, param, and filename
   *  - Kind::RuntimeLocation, Kind::RuntimeLocationPoison: a C++ file/line
   *
   * `line` will be -1 except for Kind::Known and Kind::KnownFuncParam,
   * in which case it may be a valid Hack line number.
   */
  Kind kind() const;
  const StringData* name() const;
  int32_t line() const;

  /* Unique key usable for hashing. */
  uint64_t hash() const { return m_id; }

  /* Return true if this tag is not default-constructed. */
  bool valid() const { return *this != Tag{}; }

  /*
   * Return true if this tag represents a concretely-known location
   * and should be propagated.
   *
   * i.e. if this function returns false, we treat an array with this tag
   * as needing a new tag if we get the opportunity to retag it.
   */
  bool concrete() const {
    switch (kind()) {
    case Kind::Invalid: return false;
    case Kind::Known: return true;
    case Kind::KnownTraitMerge: return true;
    case Kind::KnownLargeEnum: return true;
    case Kind::KnownFuncParam: return true;
    case Kind::RuntimeLocation: return true;
    case Kind::RuntimeLocationPoison: return false;
    }
    always_assert(false);
  }

  operator bool() const { return concrete(); }

  bool operator==(const Tag& other) const {
    return m_id == other.m_id;
  }
  bool operator!=(const Tag& other) const {
    return m_id != other.m_id;
  }

  std::string toString() const;

private:
  Tag(Kind kind, const StringData* name, int32_t line = -1);

  /* these are here since we needed to be friends with these types */
  static Tag get(const ArrayData* ad);
  static Tag get(const APCArray* a);
  static Tag get(const AsioExternalThreadEvent* ev);
  static void set(ArrayData* ad, Tag tag);
  static void set(APCArray* a, Tag tag);
  static void set(AsioExternalThreadEvent* ev, Tag tag);

  /* we are just everybody's best friend */
  friend Tag getTag(const ArrayData* a);
  friend Tag getTag(const APCArray* a);
  friend Tag getTag(const AsioExternalThreadEvent* ev);

  friend void setTag(ArrayData* a, Tag tag);
  friend void setTag(APCArray* a, Tag tag);
  friend void setTag(AsioExternalThreadEvent* ev, Tag tag);
  friend void setTagForStatic(ArrayData* a, Tag tag);

  friend void clearTag(ArrayData* ad);
  friend void clearTag(APCArray* a);
  friend void clearTag(AsioExternalThreadEvent* ev);

private:
  uint32_t m_id = 0;
};

/*
 * This is a separate struct so it can live in RDS and not be GC scanned--the
 * actual RDS-local handle is kept in the implementation
 */
struct ArrayProvenanceTable {
  /* The table itself -- allocated in general heap */
  folly::F14FastMap<const void*, Tag> tags;

  /*
   * We never dereference ArrayData*s from this table--so it's safe for the GC
   * to ignore them in this table
   */
  TYPE_SCAN_IGNORE_FIELD(tags);
};

///////////////////////////////////////////////////////////////////////////////

/*
 * Create a tag based on the current PC and unit.
 *
 * Returns an invalid tag if arrprov is off, or if we can't sync the VM regs.
 */
Tag tagFromPC();

/*
 * Create a tag based on `sk`. Returns an invalid tag if arrprov is off.
 */
Tag tagFromSK(SrcKey sk);

/*
 * RAII struct for modifying the behavior of tagFromPC().
 *
 * When this is in effect we use the tag provided instead of computing a
 * backtrace
 */
struct TagOverride {
  enum class ForceTag {};

  explicit TagOverride(Tag tag);
  TagOverride(Tag tag, ForceTag);
  ~TagOverride();

  TagOverride(TagOverride&&) = delete;
  TagOverride(const TagOverride&) = delete;

  TagOverride& operator=(const TagOverride&) = delete;
  TagOverride& operator=(TagOverride&&) = delete;

private:
  bool m_valid;
  Tag m_saved_tag;
};

#define ARRPROV_STR_IMPL(X) #X
#define ARRPROV_STR(X) ARRPROV_STR_IMPL(X)

#define ARRPROV_HERE() ([&]{                                           \
    static auto const tag = ::HPHP::arrprov::Tag::RuntimeLocation(     \
        ::HPHP::makeStaticString(__FILE__ ":" ARRPROV_STR(__LINE__))); \
    return tag;                                                        \
  }())

#define ARRPROV_HERE_POISON() ([&]{                                      \
    static auto const tag = ::HPHP::arrprov::Tag::RuntimeLocationPoison( \
        ::HPHP::makeStaticString(__FILE__ ":" ARRPROV_STR(__LINE__)));   \
    return tag;                                                          \
  }())

#define ARRPROV_USE_RUNTIME_LOCATION() \
  ::HPHP::arrprov::TagOverride ap_override(ARRPROV_HERE())

#define ARRPROV_USE_POISONED_LOCATION() \
  ::HPHP::arrprov::TagOverride ap_override(ARRPROV_HERE_POISON())

// Set tag even if provenanance is currently disabled.
// This is useful for runtime initialization and config parsing code, where
// Eval.ArrayProvenance may change as result of config parsing.
#define ARRPROV_USE_RUNTIME_LOCATION_FORCE()      \
  ::HPHP::arrprov::TagOverride ap_override(       \
      ARRPROV_HERE(),                             \
      ::HPHP::arrprov::TagOverride::ForceTag{}    \
  )

#define ARRPROV_USE_VMPC() \
  ::HPHP::arrprov::TagOverride ap_override({})

/*
 * Whether `a` admits a provenance tag.
 *
 * Depends on the ArrProv.* runtime options.
 */
bool arrayWantsTag(const ArrayData* a);
bool arrayWantsTag(const APCArray* a);
bool arrayWantsTag(const AsioExternalThreadEvent* a);

auto constexpr kAPCTagSize = 8;

/*
 * Get the provenance tag for `a`.
 */
Tag getTag(const ArrayData* a);
Tag getTag(const APCArray* a);
Tag getTag(const AsioExternalThreadEvent* ev);

/*
 * Set the provenance tag for `a` to `tag`. The ArrayData* must be
 * non-static.
 */
void setTag(ArrayData* a, Tag tag);
void setTag(APCArray* a, Tag tag);
void setTag(AsioExternalThreadEvent* ev, Tag tag);

/*
 * Like setTag(), but for static arrays. Only meant for use in
 * GetScalarArray.
 */
void setTagForStatic(ArrayData* a, Tag tag);

/*
 * Clear a tag for a released array---only call this if the array is henceforth
 * unreachable or no longer of a kind that accepts provenance tags
 */
void clearTag(ArrayData* ad);
void clearTag(APCArray* a);
void clearTag(AsioExternalThreadEvent* ev);

/*
 * Invalidates the old tag on the provided array and reassigns one from the
 * current PC, if the array still admits a tag.
 *
 * If the array no longer admits a tag, but has one set, clears it.
 *
 */
void reassignTag(ArrayData* ad);

/*
 * Produce a static array with the given provenance tag.
 *
 * If an invalid tag is provided, we attempt to make one from vmpc(), and
 * failing that we just return the input array.
 */
ArrayData* tagStaticArr(ArrayData* ad, Tag tag = {});

///////////////////////////////////////////////////////////////////////////////

namespace TagTVFlags {
constexpr int64_t TAG_PROVENANCE_HERE_MUTATE_COLLECTIONS = 1;
}

/*
 * Recursively tag the given TypedValue, tagging it (if necessary), and if it
 * is an array-like, recursively tagging its values (if necessary).
 *
 * This function will tag values within, say, a dict, even if it doesn't tag the
 * dict itself. This behavior is important because it allows us to implement
 * provenance for (nested) static arrays in ProvenanceSkipFrame functions.
 *
 * The only other type that can contain nested arrays are objects. This function
 * stops at objects, unless you use the TAG_PROVENANCE_HERE_MUTATE_COLLECTIONS
 * flag in which case it'll (recursively) tag collections values.
 *
 * This method will return a new TypedValue or modify and inc-ref `in`.
 */
TypedValue tagTvRecursively(TypedValue in, int64_t flags = 0);

/*
 * Recursively mark/unmark the given TV as being a legacy array.
 *
 * This function will recurse through array-like values. It will always stop
 * at objects, including collections.
 *
 * Attempting to mark a vec or dict pre-HADVAs triggers notices. We'll warn
 * at most once per call since extra notices hurt performance for no benefit.
 *
 * This method will return a new TypedValue or modify and inc-ref `in`.
 */
TypedValue markTvRecursively(TypedValue in, bool legacy);

/*
 * Mark/unmark the given TV as being a legacy array.
 *
 * Attempting to mark a vec or dict pre-HADVAs triggers notices.
 *
 * This method will return a new TypedValue or modify and inc-ref `in`.
 */
TypedValue markTvShallow(TypedValue in, bool legacy);

/*
 * Mark/unmark the given TV up to a fixed depth. You probably don't want to
 * use this helper, but we need it for certain constrained cases (mainly for
 * backtrace arrays, which are varrays-of-darrays-of-arbitrary-values).
 *
 * A depth of 0 means no user-provided limit. A depth of 1 is "markTvShallow".
 */
TypedValue markTvToDepth(TypedValue in, bool legacy, uint32_t depth);

///////////////////////////////////////////////////////////////////////////////

}}
